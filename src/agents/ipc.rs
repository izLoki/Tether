//! The local endpoint `tether hook` reports to: a named pipe on Windows, a unix socket
//! elsewhere. Every local terminal gets its path as `TETHER_IPC`. Messages are queued in
//! an inbox the UI drains once per frame; each one wakes the UI.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use eframe::egui;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};

use super::hook::{IpcMessage, valid_session};

/// A client may send at most this much, within [`CLIENT_TIMEOUT`].
const MAX_CLIENT_BYTES: u64 = 64 * 1024;
const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);
/// Messages kept while the UI is not draining (it drains every frame).
const MAX_QUEUED: usize = 1024;
/// Caps on what one message may carry into the UI and notifications.
const MAX_MESSAGE_CHARS: usize = 300;
const MAX_CWD_BYTES: usize = 1024;
const MAX_WORD_BYTES: usize = 128;
/// Longest wait between retries when the next pipe instance can't be created.
#[cfg(windows)]
const MAX_BACKOFF: Duration = Duration::from_secs(5);

static ENDPOINT: OnceLock<String> = OnceLock::new();

/// The path exported as `TETHER_IPC`, once the server runs.
pub fn endpoint() -> Option<&'static str> {
    ENDPOINT.get().map(String::as_str)
}

/// Environment for a new local terminal: `TETHER_IPC` and `TETHER_EXE` (empty until
/// the server runs). `TETHER_SESSION` is set by the pty itself.
pub fn session_env() -> Vec<(&'static str, String)> {
    let Some(ep) = endpoint() else { return Vec::new() };
    let mut env = vec![("TETHER_IPC", ep.to_string())];
    if let Ok(exe) = std::env::current_exe() {
        env.push(("TETHER_EXE", exe.to_string_lossy().into_owned()));
    }
    env
}

#[derive(Clone, Default)]
pub struct Inbox(Arc<Mutex<Vec<IpcMessage>>>);

impl Inbox {
    pub fn drain(&self) -> Vec<IpcMessage> {
        self.0.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default()
    }

    fn push(&self, msg: IpcMessage) {
        if let Ok(mut q) = self.0.lock()
            && q.len() < MAX_QUEUED
        {
            q.push(msg);
        }
    }
}

/// Starts the server on `rt` (once per process; later calls return a fresh, unused inbox).
pub fn start(rt: &tokio::runtime::Handle, ctx: &egui::Context) -> Inbox {
    let inbox = Inbox::default();
    if ENDPOINT.get().is_some() {
        return inbox;
    }
    let name = endpoint_name();
    let (inbox2, ctx) = (inbox.clone(), ctx.clone());
    let _guard = rt.enter();
    let started = platform::listen(&name, inbox2, ctx);
    if started.is_ok() {
        let _ = ENDPOINT.set(name);
    }
    inbox
}

/// A fresh endpoint with a 128-bit random part: on Windows the unguessable pipe name is
/// a second line of defence behind its owner-only DACL; on unix the socket sits in a
/// private (0700) directory of that name.
fn endpoint_name() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    if cfg!(windows) {
        format!(r"\\.\pipe\tether-{}-{id}", std::process::id())
    } else {
        // Darwin's sockaddr_un has only 104 bytes. The usual /var/folders TMPDIR
        // nearly fills that once the random directory and filename are appended.
        let root = if cfg!(target_os = "macos") { std::path::PathBuf::from("/tmp") } else { std::env::temp_dir() };
        root.join(format!("tether-{}-{id}", std::process::id())).join("ipc.sock").to_string_lossy().into_owned()
    }
}

/// Reads one client's lines into the inbox.
async fn serve_client(stream: impl AsyncRead + Unpin, inbox: Inbox, ctx: egui::Context) {
    let mut lines = BufReader::new(stream.take(MAX_CLIENT_BYTES)).lines();
    let read = async {
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(msg) = parse_line(&line) {
                inbox.push(msg);
                ctx.request_repaint();
            }
        }
    };
    let _ = tokio::time::timeout(CLIENT_TIMEOUT, read).await;
}

/// A valid message from one line, or `None`. Free text is capped: it ends up in the
/// agents panel and in desktop notifications.
pub fn parse_line(line: &str) -> Option<IpcMessage> {
    let mut msg: IpcMessage = serde_json::from_str(line.trim()).ok()?;
    let short = |w: &str| w.len() <= MAX_WORD_BYTES;
    if msg.v != 1 || !valid_session(&msg.session) || !short(&msg.agent) || !short(&msg.event) {
        return None;
    }
    msg.message = msg.message.map(|m| super::snippet(&m, MAX_MESSAGE_CHARS));
    msg.cwd = msg.cwd.filter(|c| c.len() <= MAX_CWD_BYTES);
    for word in [&mut msg.notification_type, &mut msg.agent_session, &mut msg.tool] {
        if word.as_deref().is_some_and(|w| !short(w)) {
            *word = None;
        }
    }
    Some(msg)
}

#[cfg(windows)]
mod platform {
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

    use super::*;

    /// Protected DACL with one entry: full access for the owner (this user) only.
    const OWNER_ONLY: &str = "D:P(A;;GA;;;OW)";

    /// One pipe instance, accessible to its owner only. Remote clients are rejected
    /// (tokio's default `reject_remote_clients`).
    pub(super) fn create(name: &str, first: bool) -> std::io::Result<NamedPipeServer> {
        let sddl: Vec<u16> = OWNER_ONLY.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: valid NUL-terminated input; on success `sd` is a LocalAlloc'd descriptor.
        if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut attrs = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd, bInheritHandle: 0 };
        // SAFETY: `attrs` and the descriptor it points to outlive the call.
        let server = unsafe { ServerOptions::new().first_pipe_instance(first).create_with_security_attributes_raw(name, (&raw mut attrs).cast()) };
        // SAFETY: freeing the descriptor allocated above, once.
        unsafe { LocalFree(sd) };
        server
    }

    /// The next instance; retries with backoff instead of giving up on the endpoint.
    async fn next_instance(name: &str) -> NamedPipeServer {
        let mut wait = Duration::from_millis(50);
        loop {
            match create(name, false) {
                Ok(server) => return server,
                Err(e) => {
                    eprintln!("tether: hook pipe: {e}; retrying in {wait:?}");
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(MAX_BACKOFF);
                }
            }
        }
    }

    pub fn listen(name: &str, inbox: Inbox, ctx: egui::Context) -> std::io::Result<()> {
        let mut server = create(name, true)?;
        let name = name.to_string();
        tokio::spawn(async move {
            loop {
                let connected = server.connect().await.is_ok();
                let client = std::mem::replace(&mut server, next_instance(&name).await);
                if connected {
                    tokio::spawn(serve_client(client, inbox.clone(), ctx.clone()));
                }
            }
        });
        Ok(())
    }
}

#[cfg(unix)]
mod platform {
    use std::os::unix::fs::PermissionsExt;

    use tokio::net::UnixListener;

    use super::*;

    pub fn listen(name: &str, inbox: Inbox, ctx: egui::Context) -> std::io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        // Bind inside a fresh directory only we can enter: no window in which another
        // user could connect before the socket's own mode is tightened.
        let dir = std::path::Path::new(name).parent().ok_or_else(|| std::io::Error::other("socket path has no folder"))?;
        std::fs::DirBuilder::new().mode(0o700).create(dir)?;
        let listener = UnixListener::bind(name)?;
        std::fs::set_permissions(name, std::fs::Permissions::from_mode(0o600))?;
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve_client(stream, inbox.clone(), ctx.clone()));
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_validated() {
        assert!(parse_line(r#"{"v":1,"session":"ab12","agent":"claude","event":"Stop"}"#).is_some());
        assert!(parse_line(r#"{"v":2,"session":"ab12","agent":"claude","event":"Stop"}"#).is_none());
        assert!(parse_line(r#"{"v":1,"session":"a/b","agent":"claude","event":"Stop"}"#).is_none());
        assert!(parse_line("not json").is_none());
    }

    #[test]
    fn free_text_is_capped() {
        let long = "x".repeat(5000);
        let line = format!(r#"{{"v":1,"session":"ab12","agent":"claude","event":"Stop","message":"{long}","cwd":"{long}","tool":"{long}"}}"#);
        let msg = parse_line(&line).unwrap();
        assert!(msg.message.unwrap().chars().count() <= MAX_MESSAGE_CHARS);
        assert_eq!((msg.cwd, msg.tool), (None, None));
        let huge_event = format!(r#"{{"v":1,"session":"ab12","agent":"claude","event":"{long}"}}"#);
        assert!(parse_line(&huge_event).is_none());
    }

    #[test]
    fn endpoints_are_unguessable_and_accepted_by_the_hook() {
        let (a, b) = (endpoint_name(), endpoint_name());
        assert_ne!(a, b);
        assert!(a.len() > 40, "{a}");
        #[cfg(target_os = "macos")]
        assert!(a.starts_with("/tmp/tether-") && a.len() < 104, "Darwin socket path too long: {a}");
        #[cfg(windows)]
        {
            assert!(super::super::hook::valid_endpoint(&a));
            assert!(!super::super::hook::valid_endpoint(r"C:\Users\me\important.txt"));
            assert!(!super::super::hook::valid_endpoint(r"\\.\pipe\other-app"));
            assert!(!super::super::hook::valid_endpoint(r"\\.\pipe\tether-..\x"));
        }
    }

    /// The real round trip: `hook::send` from a thread into the running server.
    #[test]
    fn hook_messages_reach_the_inbox() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let ctx = egui::Context::default();
        let inbox = start(rt.handle(), &ctx);
        let Some(ep) = endpoint() else { panic!("server did not start") };
        let msg = super::super::hook::build_message("claude", Some("Stop"), &serde_json::Value::Null, "cafe01").unwrap();
        let line = format!("{}\n", serde_json::to_string(&msg).unwrap());
        super::super::hook::send_for_test(ep, line.as_bytes()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut got = Vec::new();
        while got.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            got = inbox.drain();
        }
        assert_eq!(got, vec![msg]);
    }
}
