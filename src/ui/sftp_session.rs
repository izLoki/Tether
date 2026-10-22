//! The SFTP tab's connection lifecycle: the first connect, noticing a dropped session
//! (a closed transport, or a request that failed for transport reasons rather than with
//! a server status such as "permission denied"), automatic reconnects with backoff that
//! keep the current folder, and a friendly failure once retries run out.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use eframe::egui;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::StatusCode;

use super::{NavHistory, Row, SftpView, SharedHostCache, Stage, cached, learn, list_rows, set};
use crate::keystore::BadPassphrase;
use crate::profile::{Profile, secret};
use crate::settings;
use crate::sftp_history;
use crate::ssh::connection::{AuthFailed, Connection};
use crate::ssh::pool::Pool;
use crate::ssh::sftp::Sftp;
use crate::ui::sftp_data::HistoryRecorder;
use crate::ui::toast::Toasts;

/// Waits before each automatic reconnect attempt; its length is the number of attempts.
const RETRY_DELAYS: [Duration; 4] = [Duration::from_secs(1), Duration::from_secs(2), Duration::from_secs(5), Duration::from_secs(10)];
pub(super) const MAX_ATTEMPTS: u32 = RETRY_DELAYS.len() as u32;
/// One reconnect attempt that hangs this long (dead network) counts as failed.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(20);
/// How often a live session checks that its transport is still open.
const WATCH_EVERY: Duration = Duration::from_secs(1);

pub(super) enum Link {
    Connecting,
    Ready {
        sftp: Arc<Sftp>,
    },
    /// The session dropped and is being re-established automatically.
    Reconnecting {
        attempt: u32,
    },
    Failed(Failure),
}

/// What the failure card shows.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Failure {
    pub title: &'static str,
    pub message: String,
    /// The raw error chain, behind "Details".
    pub detail: String,
}

impl Failure {
    fn new(title: &'static str, err: &anyhow::Error) -> Self {
        Self { title, message: humanize(err), detail: format!("{err:#}") }
    }
}

pub(super) struct RemoteState {
    pub link: Link,
    pub cwd: String,
    pub rows: Vec<Row>,
    pub loading: bool,
    pub error: Option<String>,
    /// Bumped per listing request; results from older requests are discarded.
    pub generation: u64,
    pub nav: NavHistory<String>,
    /// `rows` still belong to the previous folder while the new one loads (shown dimmed,
    /// never blanked, so navigating doesn't flash an empty list).
    pub stale: bool,
    /// Connect progress while `link` is `Connecting` or `Reconnecting`.
    pub stage: Stage,
    /// Bumped per connect / reconnect run (and when the view closes): background tasks of
    /// an older run see the change and stop.
    pub epoch: u64,
}

impl Default for RemoteState {
    fn default() -> Self {
        Self {
            link: Link::Connecting,
            cwd: String::new(),
            rows: Vec::new(),
            loading: true,
            error: None,
            generation: 0,
            nav: NavHistory::default(),
            stale: false,
            stage: Stage::Connecting,
            epoch: 0,
        }
    }
}

/// Everything a connect or reconnect needs, cloned into its background task.
#[derive(Clone)]
pub(super) struct Session {
    pub pool: Pool,
    pub toasts: Toasts,
    pub profile: Profile,
    pub state: Arc<Mutex<RemoteState>>,
    pub cache: SharedHostCache,
    pub history: Arc<Mutex<Vec<String>>>,
    pub recorder: Arc<HistoryRecorder>,
    pub ctx: egui::Context,
    pub rt: tokio::runtime::Handle,
}

struct Opened {
    sftp: Arc<Sftp>,
    conn: Arc<Connection>,
    cwd: String,
    rows: Vec<Row>,
}

impl Session {
    /// A first connect, or the user's "Reconnect": the current folder (if any) is kept.
    pub fn start(&self, secret: Option<String>) {
        let mut epoch = 0;
        set(&self.state, |s| {
            s.epoch += 1;
            epoch = s.epoch;
            s.link = Link::Connecting;
            s.loading = true;
            s.error = None;
            s.stage = Stage::Connecting;
            s.stale = !s.rows.is_empty();
        });
        let me = self.clone();
        self.rt.spawn(async move {
            match me.open(secret).await {
                Ok(opened) => me.ready(epoch, opened, false),
                Err(e) => me.fail(epoch, Failure::new("Couldn't open SFTP", &e)),
            }
        });
    }

    /// Something noticed that `sftp` stopped working: reconnect, unless that session was
    /// already replaced (several failing requests report the same loss once).
    pub fn lost(&self, sftp: &Arc<Sftp>) {
        let mut epoch = None;
        set(&self.state, |s| {
            if !matches!(&s.link, Link::Ready { sftp: live, .. } if Arc::ptr_eq(live, sftp)) {
                return;
            }
            s.epoch += 1;
            epoch = Some(s.epoch);
            s.link = Link::Reconnecting { attempt: 1 };
            s.loading = true;
            s.error = None;
            s.stage = Stage::Connecting;
            s.stale = !s.rows.is_empty();
        });
        let Some(epoch) = epoch else { return };
        self.ctx.request_repaint();
        let me = self.clone();
        self.rt.spawn(async move { me.retry(epoch).await });
    }

    async fn retry(&self, epoch: u64) {
        let mut last = anyhow::anyhow!("The connection was lost");
        for attempt in 1..=MAX_ATTEMPTS {
            tokio::time::sleep(retry_delay(attempt).unwrap_or(WATCH_EVERY)).await;
            if !self.is_current(epoch) {
                return;
            }
            set(&self.state, |s| {
                s.link = Link::Reconnecting { attempt };
                s.stage = Stage::Connecting;
            });
            self.ctx.request_repaint();
            let secret = secret::get(self.profile.secret_id());
            let result = tokio::time::timeout(ATTEMPT_TIMEOUT, self.open(secret)).await.unwrap_or_else(|_| Err(anyhow::anyhow!("timed out reconnecting")));
            match result {
                Ok(opened) => return self.ready(epoch, opened, true),
                // Retrying can't fix a rejected or missing secret: the user has to step in.
                Err(e) if needs_user(&e) => return self.fail(epoch, Failure::new("Sign-in needed", &e)),
                Err(e) => last = e,
            }
        }
        self.fail(epoch, Failure::new("Connection lost", &last));
    }

    fn is_current(&self, epoch: u64) -> bool {
        self.state.lock().map(|s| s.epoch == epoch).unwrap_or(false)
    }

    /// Connects (or reuses the pooled transport), opens SFTP and lists the folder to show:
    /// the current one, else the last session's (if enabled), else the start folder.
    async fn open(&self, secret: Option<String>) -> Result<Opened> {
        let stage = |st: Stage| {
            set(&self.state, |s| s.stage = st);
            self.ctx.request_repaint();
        };
        if self.history.lock().map(|h| h.is_empty()).unwrap_or(false) {
            let loaded = sftp_history::load(&self.recorder.file, self.profile.id);
            set(&self.history, |h| *h = loaded);
        }
        let conn = self.pool.get_staged(&self.profile, secret, &stage).await?;
        if let Some(fp) = conn.learned_fingerprint.lock().ok().and_then(|mut f| f.take()) {
            self.toasts.info(format!("New host key trusted for {}\n{fp}", self.profile.host));
        }
        stage(Stage::Opening);
        let sftp = Sftp::open(&conn).await?;
        stage(Stage::Loading);
        let current = self.state.lock().ok().map(|s| s.cwd.clone()).filter(|c| !c.is_empty());
        let resume_dir = settings::get().sftp.resume_last_dir;
        let last = current.or_else(|| if resume_dir { self.history.lock().ok().and_then(|h| h.first().cloned()) } else { None });
        let (cwd, rows) = match self.resume(&sftp, last.as_deref()).await {
            Some(found) => found,
            None => {
                let start = self.profile.remote_start_dir.trim();
                let cwd = sftp.canonicalize(if start.is_empty() { "." } else { start }).await?;
                self.show_cached(&cwd);
                let rows = list_rows(&sftp, &cwd).await?;
                (cwd, rows)
            }
        };
        Ok(Opened { sftp, conn, cwd, rows })
    }

    /// Tries `last`; `None` (use the start folder instead) when there is none or it no
    /// longer exists.
    async fn resume(&self, sftp: &Sftp, last: Option<&str>) -> Option<(String, Vec<Row>)> {
        let cwd = sftp.canonicalize(last?).await.ok()?;
        self.show_cached(&cwd);
        let rows = list_rows(sftp, &cwd).await.ok()?;
        Some((cwd, rows))
    }

    /// Shows the last known listing of `cwd` at once while the fresh one loads.
    fn show_cached(&self, cwd: &str) {
        if let Some(rows) = cached(&self.cache, cwd) {
            set(&self.state, |s| {
                if s.cwd != cwd {
                    s.cwd = cwd.to_string();
                    s.rows = rows;
                }
            });
            self.ctx.request_repaint();
        }
    }

    fn ready(&self, epoch: u64, opened: Opened, reconnected: bool) {
        if !self.is_current(epoch) {
            return;
        }
        let Opened { sftp, conn, cwd, rows } = opened;
        learn(&self.cache, self.profile.id, &cwd, &rows, true);
        self.recorder.record(&cwd);
        set(&self.state, |s| {
            s.link = Link::Ready { sftp: sftp.clone() };
            s.cwd = cwd;
            s.rows = rows;
            s.loading = false;
            s.stale = false;
        });
        if reconnected {
            self.toasts.success(format!("Reconnected to {}", self.profile.display_name()));
        }
        self.ctx.request_repaint();
        let me = self.clone();
        self.rt.spawn(async move { me.watch(epoch, sftp, conn).await });
    }

    fn fail(&self, epoch: u64, failure: Failure) {
        if !self.is_current(epoch) {
            return;
        }
        set(&self.state, |s| {
            s.link = Link::Failed(failure);
            s.loading = false;
        });
        self.ctx.request_repaint();
    }

    /// Notices a transport that closed (server restart, keepalive timeout) while the tab
    /// sits idle, when no request would fail to tell us.
    async fn watch(&self, epoch: u64, sftp: Arc<Sftp>, conn: Arc<Connection>) {
        loop {
            tokio::time::sleep(WATCH_EVERY).await;
            if !self.is_current(epoch) {
                return;
            }
            if conn.is_closed() {
                return self.lost(&sftp);
            }
        }
    }
}

impl SftpView {
    pub(super) fn session(&self) -> Option<Session> {
        self.session.lock().ok().and_then(|s| s.clone())
    }

    /// Reports `err` from a request made with `sftp`: a lost connection starts the
    /// reconnect and returns `None` (the banner explains it); anything else is returned
    /// for the pane to show.
    pub(super) fn triage(session: Option<&Session>, sftp: &Arc<Sftp>, err: anyhow::Error) -> Option<String> {
        if !is_connection_lost(&err) {
            return Some(format!("{err:#}"));
        }
        if let Some(s) = session {
            s.lost(sftp);
        }
        None
    }

    /// Resolve a terminal link using the remote cwd and metadata, never the PC's filesystem.
    pub(super) fn resolve_location(&self, path: String, terminal: Option<crate::ssh::terminal::TerminalHandle>) {
        let (Some(sftp), Some(session)) = (self.sftp(), self.session()) else { return };
        let (term, profile, jump, jumping, ctx) = (terminal, self.profile.clone(), self.jump.clone(), self.jumping.clone(), self.ctx.clone());
        let epoch = self.location_epoch.clone();
        let request = epoch.load(Ordering::Relaxed);
        jumping.store(true, Ordering::Relaxed);
        self.rt.spawn(async move {
            let found: Result<String> = async {
                let conn = session.pool.get(&profile, secret::get(profile.secret_id())).await?;
                let home = sftp.canonicalize(".").await?;
                let cwd = if path.starts_with('/') || path.starts_with('~') {
                    home.clone()
                } else if let Some(term) = &term {
                    let reported = term.shared.lock().ok().and_then(|s| s.parser.callbacks().cwd.clone());
                    if let Some(cwd) = reported {
                        cwd
                    } else {
                        let (age_secs, rows, cols) = term.probe_facts();
                        crate::ssh::cwd::terminal_cwd(&conn, crate::ssh::cwd::Probe { tag: &term.tag, age_secs, rows, cols }).await?
                    }
                } else {
                    home.clone()
                };
                let resolved = crate::ssh::cwd::resolve_path(&conn, &sftp, &path, &cwd).await?;
                let attrs = sftp.stat(&resolved).await?;
                let directory = if attrs.is_dir() { resolved } else { crate::ssh::sftp::parent(&resolved) };
                sftp.canonicalize(&directory).await
            }
            .await;
            if epoch.load(Ordering::Relaxed) != request {
                return;
            }
            match found {
                Ok(path) => set(&jump, |j| {
                    if epoch.load(Ordering::Relaxed) == request {
                        *j = Some(path);
                    }
                }),
                Err(e) => session.toasts.error(format!("Could not open path in SFTP: {e:#}")),
            }
            jumping.store(false, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    /// Asks where the linked terminal is and navigates there (OSC 7 first, then /proc).
    pub(super) fn jump_to_terminal(&self) {
        let Some(term) = self.linked_terminal.clone() else { return };
        let epoch = self.location_epoch.clone();
        let request = epoch.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        if let Some(cwd) = term.shared.lock().ok().and_then(|s| s.parser.callbacks().cwd.clone()) {
            set(&self.jump, |j| *j = Some(cwd));
            self.jumping.store(false, Ordering::Relaxed);
            return;
        }
        let Some(session) = self.session() else { return };
        let (profile, jump, jumping, ctx) = (self.profile.clone(), self.jump.clone(), self.jumping.clone(), self.ctx.clone());
        let (age_secs, rows, cols) = term.probe_facts();
        jumping.store(true, Ordering::Relaxed);
        self.rt.spawn(async move {
            let found = async {
                // The terminal's transport is normally live; the stored secret covers a
                // dropped one instead of failing with "Password required".
                let conn = session.pool.get(&profile, secret::get(profile.secret_id())).await?;
                let probe = crate::ssh::cwd::Probe { tag: &term.tag, age_secs, rows, cols };
                crate::ssh::cwd::terminal_cwd(&conn, probe).await
            }
            .await;
            if epoch.load(Ordering::Relaxed) != request {
                return;
            }
            match found {
                Ok(path) => set(&jump, |j| {
                    if epoch.load(Ordering::Relaxed) == request {
                        *j = Some(path);
                    }
                }),
                Err(e) => session.toasts.error(format!("{e:#}")),
            }
            jumping.store(false, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }
}

/// The delay before automatic reconnect attempt `attempt` (1-based); `None` once they
/// are used up.
pub(super) fn retry_delay(attempt: u32) -> Option<Duration> {
    RETRY_DELAYS.get(usize::try_from(attempt.checked_sub(1)?).ok()?).copied()
}

/// True when `err` means the session itself is gone (closed channel or transport,
/// timeouts), as opposed to the server answering a request with an error status.
pub(super) fn is_connection_lost(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        if let Some(e) = cause.downcast_ref::<SftpError>() {
            return match e {
                SftpError::Status(s) => matches!(s.status_code, StatusCode::NoConnection | StatusCode::ConnectionLost),
                SftpError::Limited(_) | SftpError::UnexpectedPacket => false,
                SftpError::IO(_) | SftpError::Timeout | SftpError::UnexpectedBehavior(_) => true,
            };
        }
        cause.is::<russh::Error>()
    })
}

/// A rejected or missing password / passphrase: only the user can fix it.
fn needs_user(err: &anyhow::Error) -> bool {
    err.chain().any(|c| c.is::<AuthFailed>() || c.is::<BadPassphrase>())
        || matches!(err.root_cause().to_string().as_str(), "Password required" | "Passphrase required")
}

/// One plain sentence for the failure card; the raw chain goes under "Details".
pub(super) fn humanize(err: &anyhow::Error) -> String {
    if needs_user(err) {
        return "The server didn't accept the saved password or key. Reconnect to enter it again.".into();
    }
    let raw = format!("{err:#}").to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| raw.contains(w));
    let text = if has(&["host key"]) {
        // Security-relevant: keep the original wording.
        return format!("{err}");
    } else if has(&["refused"]) {
        "The server refused the connection. Is SSH running on that port?"
    } else if has(&["resolve", "no such host", "lookup", "name or service"]) {
        "Couldn't find that host name. Check the address."
    } else if has(&["timed out", "timeout"]) {
        "The server didn't answer in time. It may be offline or unreachable."
    } else if has(&["subsystem"]) {
        "The server doesn't allow SFTP for this account."
    } else if is_connection_lost(err) || has(&["reset", "closed", "broken pipe", "disconnect"]) {
        "The connection to the server dropped."
    } else {
        "The SFTP session couldn't be opened."
    };
    text.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    use russh_sftp::protocol::Status;

    fn status(code: StatusCode) -> anyhow::Error {
        anyhow::Error::new(SftpError::Status(Status { id: 1, status_code: code, error_message: "x".into(), language_tag: "en".into() })).context("opening /srv")
    }

    #[test]
    fn backoff_grows_then_stops() {
        assert_eq!(retry_delay(0), None);
        let delays: Vec<Duration> = (1..=MAX_ATTEMPTS).map(|a| retry_delay(a).unwrap()).collect();
        assert!(delays.windows(2).all(|w| w[0] < w[1]), "{delays:?}");
        assert_eq!(delays[0], Duration::from_secs(1));
        assert_eq!(retry_delay(MAX_ATTEMPTS + 1), None);
    }

    #[test]
    fn server_statuses_are_not_a_lost_connection() {
        assert!(!is_connection_lost(&status(StatusCode::PermissionDenied)));
        assert!(!is_connection_lost(&status(StatusCode::NoSuchFile)));
        assert!(!is_connection_lost(&anyhow::anyhow!("disk full")));
        assert!(is_connection_lost(&status(StatusCode::ConnectionLost)));
    }

    #[test]
    fn transport_errors_are_a_lost_connection() {
        let closed = anyhow::Error::new(SftpError::UnexpectedBehavior("RecvError: channel closed".into())).context("listing /srv");
        assert!(is_connection_lost(&closed));
        assert!(is_connection_lost(&anyhow::Error::new(SftpError::Timeout)));
        assert!(is_connection_lost(&anyhow::Error::new(russh::Error::Disconnect)));
    }

    #[test]
    fn auth_problems_need_the_user() {
        assert!(needs_user(&anyhow::Error::new(AuthFailed("password rejected".into()))));
        assert!(needs_user(&anyhow::anyhow!("Password required")));
        assert!(!needs_user(&anyhow::anyhow!("connection refused")));
    }

    #[test]
    fn messages_are_plain_and_keep_host_key_warnings() {
        assert!(humanize(&anyhow::anyhow!("Connection refused (os error 10061)")).contains("refused"));
        assert!(humanize(&anyhow::Error::new(SftpError::Timeout)).contains("in time"));
        assert!(humanize(&anyhow::anyhow!("Password required")).contains("password"));
        let key = anyhow::anyhow!("HOST KEY CHANGED for example.com");
        assert_eq!(humanize(&key), "HOST KEY CHANGED for example.com");
        assert_eq!(humanize(&anyhow::anyhow!("weird")), "The SFTP session couldn't be opened.");
    }
}
