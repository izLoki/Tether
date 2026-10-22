//! Ties the signals together: once per frame the app hands over its local terminals,
//! the hub reads what each one shows (title, bells, notifications, last output), merges
//! hook messages from the pipe and agent detection from the resource monitor, and runs
//! every session's [`Tracker`]. Tabs read the result through [`badge_for`].

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

use super::hook::IpcMessage;
use super::state::{Event, HookKind, Observation, Tracker, title_activity};
use super::{AgentKind, AgentState, ipc};
use crate::procmon::{self, Monitor, Usage};
use crate::ssh::terminal::{LinkState, TerminalHandle};

/// Two alerts for the same session closer than this are one.
const ALERT_GAP: Duration = Duration::from_secs(2);
/// Longest terminal notification text kept.
const NOTICE_CHARS: usize = 300;
/// Frame rate of the "working" pulse on badges.
const PULSE_FRAME: Duration = Duration::from_millis(100);
/// A hook-reported agent counts as gone only once a process sample this much newer
/// than the hook no longer shows it.
const HOOK_GRACE: Duration = Duration::from_secs(3);
/// A finished turn shown this long (tab visible, window focused) counts as seen: the
/// "Done" badge settles back to idle.
const DONE_SEEN_AFTER: Duration = Duration::from_secs(4);

/// A local terminal, as the app sees it this frame (borrowed: built every frame).
pub struct TermInfo<'a> {
    pub uid: u64,
    pub handle: &'a TerminalHandle,
    /// The tab is the active one of its pane.
    pub visible: bool,
    /// The tab's host / shell name, for the panel.
    pub label: &'a str,
    /// The name the user gave the tab, if any.
    pub custom: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Badge {
    pub kind: AgentKind,
    pub state: AgentState,
}

/// One row of the agents panel.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub uid: u64,
    pub kind: AgentKind,
    pub state: AgentState,
    pub since: Instant,
    pub message: Option<String>,
    /// `message` is text a program printed (OSC notification), not from agent hooks.
    pub message_from_terminal: bool,
    /// The session's name: the user's tab name, else the agent's title (`agents::title`).
    pub title: String,
    /// Folder name of the session's working directory, else the tab title.
    pub project: String,
    pub cwd: Option<String>,
    pub hooked: bool,
    pub usage: Option<Arc<Usage>>,
    pub visible: bool,
}

/// A session just started needing the user.
#[derive(Debug, Clone)]
pub struct Alert {
    pub uid: u64,
    pub kind: AgentKind,
    pub state: AgentState,
    pub project: String,
    pub message: Option<String>,
    /// `message` is text a program printed (OSC notification), not from agent hooks.
    pub from_terminal: bool,
    /// The tab was on screen when it happened.
    pub visible: bool,
}

static BADGES: Mutex<Option<HashMap<u64, Badge>>> = Mutex::new(None);
/// The folder agents reported through their hooks, by tab.
static CWDS: Mutex<Option<HashMap<u64, String>>> = Mutex::new(None);

/// The badge of tab `uid`, if an agent runs in it.
pub fn badge_for(uid: u64) -> Option<Badge> {
    BADGES.lock().ok().and_then(|b| b.as_ref().and_then(|m| m.get(&uid).copied()))
}

/// The working folder the agent in tab `uid` reported (hooks), if any.
pub fn agent_cwd(uid: u64) -> Option<String> {
    CWDS.lock().ok().and_then(|c| c.as_ref().and_then(|m| m.get(&uid).cloned()))
}

struct Session {
    tag: String,
    pid: Option<u32>,
    tracker: Tracker,
    kind: Option<AgentKind>,
    hook_kind: Option<AgentKind>,
    hook_at: Option<Instant>,
    bells: u64,
    notices: u64,
    cwd: Option<String>,
    label: String,
    visible: bool,
    /// When the finished turn was first on screen (for [`DONE_SEEN_AFTER`]).
    done_seen: Option<Instant>,
    ended: bool,
    last_alert: Option<Instant>,
    /// The terminal's raw title and OSC 7 folder as last read (copied only on change).
    seen_title: String,
    seen_cwd: Option<String>,
}

impl Session {
    fn new(tag: String, bells: u64, notices: u64, now: Instant) -> Self {
        Self {
            tag,
            pid: None,
            tracker: Tracker::new(now),
            kind: None,
            hook_kind: None,
            hook_at: None,
            bells,
            notices,
            cwd: None,
            label: String::new(),
            visible: false,
            done_seen: None,
            ended: false,
            last_alert: None,
            seen_title: String::new(),
            seen_cwd: None,
        }
    }

    fn project(&self) -> String {
        self.cwd
            .as_deref()
            .map(|c| c.trim_end_matches(['/', '\\']))
            .and_then(|c| c.rsplit(['/', '\\']).next())
            .filter(|f| !f.is_empty())
            .map_or_else(|| self.label.clone(), str::to_string)
    }
}

/// What a terminal shows, read under one short lock. Strings are copied only when
/// they changed since `prev` (this runs every frame for every local terminal).
struct Seen {
    /// New title / folder, `None` when unchanged.
    title: Option<String>,
    cwd: Option<Option<String>>,
    bells: u64,
    notices: u64,
    /// The latest notification, only when there are new ones.
    notice: Option<String>,
    last_output: Option<Instant>,
    pid: Option<u32>,
    live: bool,
    /// The shell ended (closed or failed), with its exit code if it reported one.
    ended: Option<Option<u32>>,
}

fn read(handle: &TerminalHandle, prev: Option<&Session>) -> Option<Seen> {
    // Never wait on a busy reader: this terminal is simply read next frame.
    let s = handle.shared.try_lock().ok()?;
    let ev = s.parser.callbacks();
    let title = ev.title.as_deref().unwrap_or_default();
    let changed_title = prev.is_none_or(|p| p.seen_title != title).then(|| title.to_string());
    let changed_cwd = prev.is_none_or(|p| p.seen_cwd != ev.cwd).then(|| ev.cwd.clone());
    let new_notices = prev.is_none_or(|p| ev.notice_count > p.notices);
    Some(Seen {
        title: changed_title,
        cwd: changed_cwd,
        bells: ev.bell_count,
        notices: ev.notice_count,
        notice: if new_notices { ev.notice.clone() } else { None },
        last_output: s.last_output,
        pid: s.pid,
        live: s.state.is_live(),
        ended: matches!(s.state, LinkState::Closed(_) | LinkState::Failed(_)).then(|| exit_code(&s.state)),
    })
}

/// `Process exited with code 3` -> `Some(3)`.
fn exit_code(link: &LinkState) -> Option<u32> {
    match link {
        LinkState::Closed(reason) => reason.strip_prefix("Process exited with code ").and_then(|c| c.trim().parse().ok()),
        _ => None,
    }
}

pub struct AgentHub {
    sessions: HashMap<u64, Session>,
    inbox: ipc::Inbox,
    monitor: Arc<Monitor>,
    /// [`Self::sessions`], rebuilt only when what it shows changed.
    listed: Vec<SessionInfo>,
    listed_key: u64,
    /// What [`badge_for`] last published.
    badges_key: u64,
}

impl AgentHub {
    /// Starts the hook pipe and the resource monitor.
    pub fn new(rt: &tokio::runtime::Handle, ctx: &egui::Context) -> Self {
        Self { sessions: HashMap::new(), inbox: ipc::start(rt, ctx), monitor: Arc::new(Monitor::start(ctx)), listed: Vec::new(), listed_key: 0, badges_key: 0 }
    }

    pub fn monitor(&self) -> &Monitor {
        &self.monitor
    }

    /// The user typed into tab `uid`.
    pub fn note_input(&mut self, uid: u64, now: Instant) {
        if let Some(s) = self.sessions.get_mut(&uid) {
            s.tracker.apply(Event::Input, now);
        }
    }

    /// Runs once per frame. Returns the sessions that just started needing attention.
    pub fn frame(&mut self, ctx: &egui::Context, terms: Vec<TermInfo<'_>>, now: Instant) -> Vec<Alert> {
        let mut alerts = Vec::new();
        let focused = ctx.input(|i| i.focused);
        self.drain_inbox(now, &mut alerts);
        let snap = procmon::latest();
        let mut watch = HashMap::new();
        let mut live = HashSet::new();
        for t in terms {
            live.insert(t.uid);
            let prev = self.sessions.get(&t.uid).filter(|s| s.tag == t.handle.tag);
            let Some(seen) = read(t.handle, prev) else {
                // Busy this frame: keep watching what we knew.
                if let Some(pid) = self.sessions.get(&t.uid).and_then(|s| s.pid) {
                    watch.insert(t.uid, pid);
                }
                continue;
            };
            let pid = seen.pid.filter(|_| seen.live);
            if let Some(pid) = pid {
                watch.insert(t.uid, pid);
            }
            let fresh = || Session::new(t.handle.tag.clone(), seen.bells, seen.notices, now);
            let session = self.sessions.entry(t.uid).or_insert_with(fresh);
            if session.tag != t.handle.tag {
                // Respawned in place: a new shell, a new session.
                *session = fresh();
            }
            session.pid = pid;
            if let Some(title) = seen.title {
                session.seen_title = title;
            }
            if let Some(cwd) = seen.cwd {
                session.seen_cwd = cwd;
            }
            session.visible = t.visible;
            if session.seen_cwd.is_some() && session.hook_kind.is_none() && session.cwd != session.seen_cwd {
                session.cwd.clone_from(&session.seen_cwd);
            }
            let usage = snap.as_ref().and_then(|s| s.per_tab.get(&t.uid));
            let sample = usage.and_then(|u| snap.as_ref().map(|s| (u.agent, s.at.unwrap_or(now))));
            let subprocs_busy = usage.is_some_and(|u| u.subprocs_busy);
            if session.seen_cwd.is_none()
                && session.hook_kind.is_none()
                && let Some(proc_cwd) = usage.and_then(|u| u.cwd.as_ref())
                && session.cwd.as_ref() != Some(proc_cwd)
            {
                session.cwd = Some(proc_cwd.clone());
            }
            update_kind(session, sample);
            if let Some(kind) = session.kind {
                // The agent's own title ("✳ Fix the login bug") names the task best.
                let cwd = session.cwd.as_deref().or(session.seen_cwd.as_deref());
                let label = match t.custom {
                    Some(name) => name.to_string(),
                    None => super::title::session_title(kind, Some(&session.seen_title), cwd),
                };
                if session.label != label {
                    session.label = label;
                }
            } else if session.label != t.label {
                session.label = t.label.to_string();
            }
            let Some(kind) = session.kind else {
                session.bells = seen.bells;
                session.notices = seen.notices;
                continue;
            };
            let mut transitions = Vec::new();
            if seen.bells > session.bells {
                transitions.extend(session.tracker.apply(Event::Bell, now));
            }
            if seen.notices > session.notices
                && let Some(text) = seen.notice
            {
                // Program output: keep it short (it reaches the panel and notifications).
                transitions.extend(session.tracker.apply(Event::Notify(super::snippet(&text, NOTICE_CHARS)), now));
            }
            session.bells = seen.bells;
            session.notices = seen.notices;
            if let Some(code) = seen.ended
                && !session.ended
            {
                session.ended = true;
                transitions.extend(session.tracker.apply(Event::Exited(code), now));
            }
            let obs = Observation { title_busy: title_activity(Some(kind), &session.seen_title), last_output: seen.last_output, subprocs_busy };
            transitions.extend(session.tracker.observe(obs, now));
            transitions.extend(settle_done(session, focused, now));
            for (_, to) in transitions {
                push_alert(&mut alerts, t.uid, session, to, now);
            }
        }
        self.sessions.retain(|uid, _| live.contains(uid));
        self.monitor.set_watch(watch);
        self.schedule(ctx, now);
        if self.publish_badges() {
            // One more pass, so a badge that just started working is painted and pulses.
            ctx.request_repaint();
        }
        self.refresh_list(snap.as_deref());
        alerts
    }

    fn drain_inbox(&mut self, now: Instant, alerts: &mut Vec<Alert>) {
        for msg in self.inbox.drain() {
            let Some((&uid, session)) = self.sessions.iter_mut().find(|(_, s)| s.tag == msg.session) else { continue };
            if let Some(to) = apply_hook(session, &msg, now) {
                push_alert(alerts, uid, session, to, now);
            }
        }
    }

    fn schedule(&self, ctx: &egui::Context, now: Instant) {
        let done_fade = self.sessions.values().filter_map(|s| s.done_seen.map(|t| DONE_SEEN_AFTER.saturating_sub(now.saturating_duration_since(t))));
        let recheck = self.sessions.values().filter(|s| s.kind.is_some()).filter_map(|s| s.tracker.recheck_in(now)).chain(done_fade).min();
        // Animate only while a working badge is actually being painted (last pass).
        let painted = crate::ui::agent_badge::take_pulse_painted();
        let pulsing = painted
            && crate::settings::snapshot().agents.badges
            && crate::theme::anim_time(1.0) > 0.0
            && self.sessions.values().any(|s| s.kind.is_some() && s.tracker.state == AgentState::Working);
        let next = match (recheck, pulsing) {
            (Some(r), true) => Some(r.min(PULSE_FRAME)),
            (Some(r), false) => Some(r),
            (None, true) => Some(PULSE_FRAME),
            (None, false) => None,
        };
        if let Some(after) = next {
            ctx.request_repaint_after(after);
        }
    }

    /// Publishes the tab badges when they changed; true if they did.
    fn publish_badges(&mut self) -> bool {
        let badges = || self.sessions.iter().filter_map(|(uid, s)| s.kind.map(|kind| (*uid, Badge { kind, state: s.tracker.state }, s.cwd.as_deref())));
        // Order-independent: the map's iteration order is not stable.
        let key = badges().fold(1u64, |acc, b| acc.wrapping_add(hash_of(&b)));
        if key == self.badges_key {
            return false;
        }
        let map = badges().map(|(uid, b, _)| (uid, b)).collect();
        if let Ok(mut b) = BADGES.lock() {
            *b = Some(map);
        }
        let cwds = badges().filter_map(|(uid, _, cwd)| Some((uid, cwd?.to_string()))).collect();
        if let Ok(mut c) = CWDS.lock() {
            *c = Some(cwds);
        }
        self.badges_key = key;
        true
    }

    /// Rebuilds the [`Self::sessions`] list if anything it shows changed.
    fn refresh_list(&mut self, snap: Option<&procmon::Snapshot>) {
        let mut h = DefaultHasher::new();
        snap.map(|s| s.seq).hash(&mut h);
        let mut parts: Vec<u64> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.kind.is_some())
            .map(|(uid, s)| hash_of(&(uid, s.kind, s.tracker.state, s.tracker.since, &s.tracker.message, &s.cwd, &s.label, s.tracker.hooked, s.visible)))
            .collect();
        parts.sort_unstable();
        parts.hash(&mut h);
        let key = h.finish();
        if key == self.listed_key {
            return;
        }
        self.listed_key = key;
        let mut out: Vec<SessionInfo> = self
            .sessions
            .iter()
            .filter_map(|(uid, s)| {
                Some(SessionInfo {
                    uid: *uid,
                    kind: s.kind?,
                    state: s.tracker.state,
                    since: s.tracker.since,
                    message: s.tracker.message.clone(),
                    message_from_terminal: s.tracker.message_from_terminal,
                    title: s.label.clone(),
                    project: s.project(),
                    cwd: s.cwd.clone(),
                    hooked: s.tracker.hooked,
                    usage: snap.and_then(|p| p.per_tab.get(uid).cloned()),
                    visible: s.visible,
                })
            })
            .collect();
        out.sort_by_key(|s| (s.state.urgency(), s.since, s.uid));
        self.listed = out;
    }

    /// Every agent session, most urgent first (as of the last [`Self::frame`]).
    pub fn sessions(&self) -> &[SessionInfo] {
        &self.listed
    }

    /// The next session needing attention after `current` (wrapping), most urgent first.
    pub fn next_attention(&self, current: Option<u64>) -> Option<u64> {
        let list: Vec<u64> = self.sessions().iter().filter(|s| s.state.needs_attention()).map(|s| s.uid).collect();
        let at = current.and_then(|c| list.iter().position(|u| *u == c));
        match at {
            Some(i) => list.get((i + 1) % list.len()).copied(),
            None => list.first().copied(),
        }
    }

    /// "Mark as read": the session's attention state (done, waiting...) is dealt with.
    pub fn mark_read(&mut self, uid: u64) {
        if let Some(s) = self.sessions.get_mut(&uid) {
            s.tracker.acknowledge(Instant::now());
            s.done_seen = None;
        }
    }

    pub fn attention_count(&self) -> usize {
        self.sessions.values().filter(|s| s.kind.is_some() && s.tracker.state.needs_attention()).count()
    }
}

fn hash_of(value: &impl Hash) -> u64 {
    let mut h = DefaultHasher::new();
    value.hash(&mut h);
    h.finish()
}

/// Picks the session's agent: what runs in its process tree, else what its hooks said
/// (until a newer process sample shows the agent gone).
fn update_kind(session: &mut Session, sample: Option<(Option<AgentKind>, Instant)>) {
    let gone_after_hook = matches!((sample, session.hook_at), (Some((None, at)), Some(h)) if at > h + HOOK_GRACE);
    if gone_after_hook {
        session.hook_kind = None;
        session.hook_at = None;
    }
    let kind = sample.and_then(|(k, _)| k).or(session.hook_kind);
    if kind.is_none() && session.kind.is_some() {
        // The agent quit: forget its state, the shell is just a shell again.
        let now = Instant::now();
        session.tracker = Tracker::new(now);
        session.cwd = None;
    }
    session.kind = kind;
}

/// A "Done" that has been on screen for a while settles to idle.
fn settle_done(session: &mut Session, window_focused: bool, now: Instant) -> Option<(AgentState, AgentState)> {
    if session.tracker.state != AgentState::Done || !(session.visible && window_focused) {
        session.done_seen = None;
        return None;
    }
    let seen = *session.done_seen.get_or_insert(now);
    if now.saturating_duration_since(seen) < DONE_SEEN_AFTER {
        return None;
    }
    session.done_seen = None;
    session.tracker.acknowledge(now)
}

fn apply_hook(session: &mut Session, msg: &IpcMessage, now: Instant) -> Option<AgentState> {
    let kind = HookKind::parse(&msg.event, msg.notification_type.as_deref(), msg.message.as_deref())?;
    if let Some(agent) = AgentKind::from_id(&msg.agent) {
        session.hook_kind = Some(agent);
        session.hook_at = Some(now);
        session.kind.get_or_insert(agent);
    }
    if msg.cwd.is_some() {
        session.cwd.clone_from(&msg.cwd);
    }
    let message = match kind {
        HookKind::SubagentStop => Some("Subagent finished".to_string()),
        _ => msg.message.clone(),
    };
    session.tracker.apply(Event::Hook { kind, message }, now).map(|(_, to)| to)
}

fn push_alert(alerts: &mut Vec<Alert>, uid: u64, session: &mut Session, to: AgentState, now: Instant) {
    let Some(kind) = session.kind else { return };
    if !to.needs_attention() || session.last_alert.is_some_and(|t| now.saturating_duration_since(t) < ALERT_GAP) {
        return;
    }
    session.last_alert = Some(now);
    alerts.push(Alert {
        uid,
        kind,
        state: to,
        project: session.project(),
        message: session.tracker.message.clone(),
        from_terminal: session.tracker.message_from_terminal,
        visible: session.visible,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(event: &str, extra: impl FnOnce(&mut IpcMessage)) -> IpcMessage {
        let mut m = IpcMessage {
            v: 1,
            session: "tag".into(),
            agent: "claude".into(),
            event: event.into(),
            notification_type: None,
            message: None,
            cwd: Some("D:/work/api-server".into()),
            agent_session: None,
            tool: None,
        };
        extra(&mut m);
        m
    }

    #[test]
    fn hook_messages_drive_a_session() {
        let now = Instant::now();
        let mut s = Session::new("tag".into(), 0, 0, now);
        assert_eq!(apply_hook(&mut s, &msg("SessionStart", |_| {}), now), None);
        assert_eq!(s.kind, Some(AgentKind::Claude));
        assert_eq!(s.project(), "api-server");
        assert_eq!(apply_hook(&mut s, &msg("UserPromptSubmit", |m| m.message = Some("add tests".into())), now), Some(AgentState::Working));
        let perm = msg("Notification", |m| {
            m.notification_type = Some("permission_prompt".into());
            m.message = Some("Claude needs your permission to use Bash".into());
        });
        assert_eq!(apply_hook(&mut s, &perm, now), Some(AgentState::NeedsPermission));
        assert_eq!(s.tracker.message.as_deref(), Some("Claude needs your permission to use Bash"));
        assert_eq!(apply_hook(&mut s, &msg("PreToolUse", |_| {}), now), None, "unknown events are ignored");
        let mut alerts = Vec::new();
        push_alert(&mut alerts, 1, &mut s, AgentState::NeedsPermission, now);
        push_alert(&mut alerts, 1, &mut s, AgentState::Done, now + Duration::from_millis(500));
        push_alert(&mut alerts, 1, &mut s, AgentState::Working, now + Duration::from_secs(5));
        assert_eq!(alerts.len(), 1, "rate limited, and work is not an alert");
    }

    #[test]
    fn codex_notify_updates_its_session_and_alert_without_touching_claude() {
        let now = Instant::now();
        let mut codex = Session::new("codex-tag".into(), 0, 0, now);
        let mut claude = Session::new("claude-tag".into(), 0, 0, now);
        apply_hook(&mut claude, &msg("UserPromptSubmit", |_| {}), now);
        let payload = serde_json::json!({"type":"agent-turn-complete", "thread-id":"thread-1", "cwd":"D:/work/api-server", "last-assistant-message":"Completed the refactor"});
        let message = super::super::hook::build_message("codex", None, &payload, "codex-tag").unwrap();
        let to = apply_hook(&mut codex, &message, now).unwrap();
        assert_eq!(codex.kind, Some(AgentKind::Codex));
        assert_eq!(codex.project(), "api-server");
        assert_eq!(codex.tracker.message.as_deref(), Some("Completed the refactor"));
        assert_eq!(claude.tracker.state, AgentState::Working);
        let mut alerts = Vec::new();
        push_alert(&mut alerts, 42, &mut codex, to, now);
        assert_eq!((alerts[0].uid, alerts[0].kind, alerts[0].state), (42, AgentKind::Codex, AgentState::Done));
    }

    #[test]
    fn agent_kind_follows_the_process_tree() {
        let t0 = Instant::now();
        let mut s = Session::new("tag".into(), 0, 0, t0);
        update_kind(&mut s, Some((Some(AgentKind::Codex), t0)));
        assert_eq!(s.kind, Some(AgentKind::Codex));
        s.tracker.apply(Event::Bell, t0);
        update_kind(&mut s, Some((None, t0)));
        assert_eq!(s.kind, None);
        assert_eq!(s.tracker.state, AgentState::Idle, "state is reset when the agent quits");
        // A hook seen before the next process sample keeps the session alive...
        apply_hook(&mut s, &msg("SessionStart", |_| {}), t0);
        update_kind(&mut s, Some((None, t0 + Duration::from_secs(1))));
        assert_eq!(s.kind, Some(AgentKind::Claude));
        // ...until a sample well after the hook shows no agent.
        update_kind(&mut s, Some((None, t0 + Duration::from_secs(10))));
        assert_eq!(s.kind, None);
    }

    #[test]
    fn reads_copy_strings_only_when_they_change() {
        let (term, _rx) = TerminalHandle::new(24, 80);
        if let Ok(mut s) = term.shared.lock() {
            s.parser.process(b"\x1b]0;build\x07\x1b]9;done\x07");
        }
        let first = read(&term, None).unwrap();
        assert_eq!(first.title.as_deref(), Some("build"));
        assert_eq!(first.notice.as_deref(), Some("done"));
        let mut s = Session::new(term.tag.clone(), 0, first.notices, Instant::now());
        s.seen_title = "build".into();
        let again = read(&term, Some(&s)).unwrap();
        assert!(again.title.is_none() && again.cwd.is_none() && again.notice.is_none(), "nothing new: nothing copied");
        assert_eq!(again.ended, None);
    }

    #[test]
    fn done_settles_after_being_seen() {
        let t0 = Instant::now();
        let mut s = Session::new("tag".into(), 0, 0, t0);
        apply_hook(&mut s, &msg("Stop", |_| {}), t0);
        assert_eq!(s.tracker.state, AgentState::Done);
        // Hidden or unfocused: stays done.
        assert_eq!(settle_done(&mut s, true, t0), None);
        s.visible = true;
        assert_eq!(settle_done(&mut s, false, t0 + Duration::from_secs(9)), None);
        assert_eq!(settle_done(&mut s, true, t0 + Duration::from_secs(10)), None, "first seen now");
        assert_eq!(settle_done(&mut s, true, t0 + Duration::from_secs(12)), None);
        assert_eq!(settle_done(&mut s, true, t0 + Duration::from_secs(15)), Some((AgentState::Done, AgentState::Idle)));
    }

    #[test]
    fn exit_codes_parse() {
        assert_eq!(exit_code(&LinkState::Closed("Process exited with code 2".into())), Some(2));
        assert_eq!(exit_code(&LinkState::Closed("Closed".into())), None);
        assert_eq!(exit_code(&LinkState::Connected), None);
    }
}
