//! The per-session state machine. Pure: time is passed in, so every rule is unit tested.
//!
//! Explicit signals (hook events, OSC notifications, bells, the user typing) move the
//! state at once. Heuristics (window title spinners, output activity) only fill the gaps:
//! a session with hooks installed ignores output activity, since hooks say more.

use std::time::{Duration, Instant};

use super::AgentKind;

/// Output must keep flowing this long (with no typing) before it counts as work.
pub const BURST_MIN: Duration = Duration::from_millis(1000);
/// A gap longer than this ends an output burst.
pub const BURST_GAP: Duration = Duration::from_millis(1500);
/// Typing echoes output: activity this soon after a key press is the user's, not the agent's.
pub const INPUT_QUIET: Duration = Duration::from_millis(1000);
/// Silence after activity-detected work that marks the turn finished.
pub const QUIET_DONE: Duration = Duration::from_millis(3000);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentState {
    Idle,
    Working,
    NeedsInput,
    NeedsPermission,
    Done,
    Error,
}

impl AgentState {
    pub fn label(self) -> &'static str {
        match self {
            AgentState::Idle => "Idle",
            AgentState::Working => "Working",
            AgentState::NeedsInput => "Waiting for input",
            AgentState::NeedsPermission => "Needs permission",
            AgentState::Done => "Done",
            AgentState::Error => "Error",
        }
    }

    /// States the user should look at (notifications, Ctrl+Shift+A).
    pub fn needs_attention(self) -> bool {
        matches!(self, AgentState::NeedsInput | AgentState::NeedsPermission | AgentState::Done | AgentState::Error)
    }

    /// Sort key: most urgent first.
    pub fn urgency(self) -> u8 {
        match self {
            AgentState::NeedsPermission => 0,
            AgentState::NeedsInput => 1,
            AgentState::Error => 2,
            AgentState::Done => 3,
            AgentState::Working => 4,
            AgentState::Idle => 5,
        }
    }
}

/// What a hook (or Codex' `notify`) reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookKind {
    SessionStart,
    PromptSubmit,
    /// Claude's Notification hook: a permission prompt, or idle waiting for input.
    Notification {
        permission: bool,
    },
    Stop,
    SubagentStart,
    SubagentStop,
    ToolActivity,
    SessionEnd,
    /// Codex `agent-turn-complete`.
    TurnComplete,
}

impl HookKind {
    /// Maps a hook event name (Claude's `hook_event_name`, our CLI argument, Codex' `type`).
    pub fn parse(event: &str, notification_type: Option<&str>, message: Option<&str>) -> Option<Self> {
        let e = event.to_ascii_lowercase().replace(['-', '_'], "");
        Some(match e.as_str() {
            "sessionstart" => HookKind::SessionStart,
            "userpromptsubmit" | "promptsubmit" => HookKind::PromptSubmit,
            "notification" => HookKind::Notification { permission: is_permission(notification_type, message) },
            "permissionrequest" => HookKind::Notification { permission: true },
            "stop" => HookKind::Stop,
            "subagentstart" => HookKind::SubagentStart,
            "subagentstop" => HookKind::SubagentStop,
            "posttooluse" | "posttoolusefailure" => HookKind::ToolActivity,
            "sessionend" => HookKind::SessionEnd,
            "agentturncomplete" | "turncomplete" => HookKind::TurnComplete,
            _ => return None,
        })
    }
}

fn is_permission(notification_type: Option<&str>, message: Option<&str>) -> bool {
    if let Some(t) = notification_type {
        return t.contains("permission");
    }
    message.is_some_and(|m| {
        let m = m.to_ascii_lowercase();
        m.contains("permission") || m.contains("approve") || m.contains("allow ")
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Hook {
        kind: HookKind,
        message: Option<String>,
    },
    /// An OSC 9 / OSC 777 desktop notification printed by the program.
    Notify(String),
    Bell,
    /// The user typed into the terminal.
    Input,
    /// The shell ended; `Some(code)` when it reported one.
    Exited(Option<u32>),
}

/// What the hub sees of the terminal each frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct Observation {
    /// From the window title: `Some(true)` = a busy spinner, `Some(false)` = the idle mark.
    pub title_busy: Option<bool>,
    /// When the terminal last printed anything.
    pub last_output: Option<Instant>,
    /// True when the agent process has active child processes (subagents, shells, scripts,
    /// tools) or significant CPU work in its process tree.
    pub subprocs_busy: bool,
}

/// `(from, to)` whenever the state changes.
pub type Transition = (AgentState, AgentState);

#[derive(Debug, Clone)]
pub struct Tracker {
    pub state: AgentState,
    /// When `state` was entered.
    pub since: Instant,
    /// Hook events have arrived for this session, including completion-only notify.
    pub hooked: bool,
    /// Full lifecycle hooks suppress heuristics; Codex notify only reports completion.
    lifecycle_hooks: bool,
    /// Last notification / prompt / message, for the agents panel.
    pub message: Option<String>,
    /// `message` came from terminal output (OSC 9/777/99), not from the agent's hooks.
    pub message_from_terminal: bool,
    last_input: Option<Instant>,
    burst_start: Option<Instant>,
    prev_output: Option<Instant>,
    prev_title_busy: Option<bool>,
    /// The current `Working` came from output activity (so silence can end it).
    by_activity: bool,
    /// The current `Working` is held open by active child processes (shells/scripts/subagents).
    by_subprocs: bool,
    /// Whether child processes (shells/scripts/subagents) are currently running under the agent.
    subprocs_busy: bool,
    /// Count of subagents currently active from `SubagentStart`..`SubagentStop`.
    active_subagents: u32,
}

impl Tracker {
    pub fn new(now: Instant) -> Self {
        Self {
            state: AgentState::Idle,
            since: now,
            hooked: false,
            lifecycle_hooks: false,
            message: None,
            message_from_terminal: false,
            last_input: None,
            burst_start: None,
            prev_output: None,
            prev_title_busy: None,
            by_activity: false,
            by_subprocs: false,
            subprocs_busy: false,
            active_subagents: 0,
        }
    }

    fn set(&mut self, to: AgentState, now: Instant) -> Option<Transition> {
        self.by_activity = false;
        self.by_subprocs = false;
        if self.state == to {
            return None;
        }
        let from = std::mem::replace(&mut self.state, to);
        self.since = now;
        Some((from, to))
    }

    pub fn apply(&mut self, event: Event, now: Instant) -> Option<Transition> {
        match event {
            Event::Input => {
                self.last_input = Some(now);
                // Typing answers whatever was asked: the attention is dealt with.
                if self.state.needs_attention() { self.set(AgentState::Idle, now) } else { None }
            }
            Event::Bell => match self.state {
                // Child scripts and shells often ring `\x07` while Claude is working; never
                // let a bell interrupt a hooked turn, an active spinner, or running subprocesses.
                AgentState::Idle if !self.subprocs_busy && self.active_subagents == 0 => self.set(AgentState::NeedsInput, now),
                AgentState::Working if !self.hooked && !self.subprocs_busy && self.active_subagents == 0 && self.prev_title_busy != Some(true) => {
                    self.set(AgentState::NeedsInput, now)
                }
                _ => None,
            },
            Event::Notify(text) => {
                let lower = text.to_ascii_lowercase();
                let to = if is_permission(None, Some(&text)) {
                    AgentState::NeedsPermission
                } else if ["complete", "finished", "done"].iter().any(|w| lower.contains(w)) {
                    AgentState::Done
                } else {
                    AgentState::NeedsInput
                };
                // Ignore non-permission terminal notifications while child processes or subagents are still running.
                if to != AgentState::NeedsPermission
                    && self.state == AgentState::Working
                    && (self.subprocs_busy || self.active_subagents > 0 || self.prev_title_busy == Some(true))
                {
                    return None;
                }
                self.message = Some(text);
                self.message_from_terminal = true;
                self.set(to, now)
            }
            Event::Hook { kind, message } => {
                self.hooked = !matches!(kind, HookKind::SessionEnd);
                match kind {
                    HookKind::SessionEnd => self.lifecycle_hooks = false,
                    HookKind::TurnComplete => {}
                    _ => self.lifecycle_hooks = true,
                }
                if let Some(m) = message.filter(|m| !m.trim().is_empty()) {
                    self.message = Some(m);
                    self.message_from_terminal = false;
                }
                match kind {
                    HookKind::SessionStart | HookKind::SessionEnd => {
                        self.active_subagents = 0;
                        self.set(AgentState::Idle, now)
                    }
                    HookKind::PromptSubmit | HookKind::ToolActivity => self.set(AgentState::Working, now),
                    HookKind::SubagentStart => {
                        self.active_subagents = self.active_subagents.saturating_add(1);
                        self.set(AgentState::Working, now)
                    }
                    HookKind::Notification { permission: true } => self.set(AgentState::NeedsPermission, now),
                    HookKind::Notification { permission: false } => {
                        // Do not mark as NeedsInput if subagents, child scripts, or the spinner are still active.
                        if self.subprocs_busy || self.active_subagents > 0 || self.prev_title_busy == Some(true) {
                            return None;
                        }
                        self.set(AgentState::NeedsInput, now)
                    }
                    HookKind::Stop | HookKind::TurnComplete => {
                        self.active_subagents = 0;
                        if self.subprocs_busy {
                            let t = self.set(AgentState::Working, now);
                            self.by_subprocs = true;
                            return t;
                        }
                        self.set(AgentState::Done, now)
                    }
                    HookKind::SubagentStop => {
                        self.active_subagents = self.active_subagents.saturating_sub(1);
                        // If a subagent finished while the session was mistakenly marked Done or NeedsInput,
                        // parent Claude is now continuing its turn.
                        if matches!(self.state, AgentState::Done | AgentState::NeedsInput) { self.set(AgentState::Working, now) } else { None }
                    }
                }
            }
            Event::Exited(code) => {
                self.active_subagents = 0;
                match code {
                    Some(0) => self.set(AgentState::Done, now),
                    _ => self.set(AgentState::Error, now),
                }
            }
        }
    }

    /// The user has seen what the session wanted (opened it, "Mark as read"): an
    /// attention state settles to idle.
    pub fn acknowledge(&mut self, now: Instant) -> Option<Transition> {
        if self.state.needs_attention() { self.set(AgentState::Idle, now) } else { None }
    }

    /// Applies the heuristics to what the terminal shows now.
    pub fn observe(&mut self, obs: Observation, now: Instant) -> Option<Transition> {
        self.subprocs_busy = obs.subprocs_busy;
        if let Some(t) = obs.last_output
            && self.prev_output != Some(t)
        {
            if self.prev_output.is_none_or(|p| t.saturating_duration_since(p) > BURST_GAP) {
                self.burst_start = Some(t);
            }
            self.prev_output = Some(t);
        }
        // While child processes (shells, scripts, subagents) are running under the agent,
        // keep the activity clock warm and keep the session in `Working` (unless awaiting permission).
        if obs.subprocs_busy {
            self.prev_output = Some(now);
            if self.state != AgentState::Working && self.state != AgentState::NeedsPermission {
                let t = self.set(AgentState::Working, now);
                self.by_subprocs = true;
                return t;
            }
        }
        let prev_busy = std::mem::replace(&mut self.prev_title_busy, obs.title_busy);
        match obs.title_busy {
            // Whenever the braille spinner is spinning in the title, the agent is working
            // (even if it was previously marked Done or NeedsInput).
            Some(true) if self.state != AgentState::Working && self.state != AgentState::NeedsPermission => {
                return self.set(AgentState::Working, now);
            }
            // Only let `✳` (title_busy == Some(false)) mark Done when hooks are NOT active
            // and no child processes or subagents are still running.
            Some(false)
                if prev_busy == Some(true)
                    && self.state == AgentState::Working
                    && !self.lifecycle_hooks
                    && !obs.subprocs_busy
                    && self.active_subagents == 0 =>
            {
                return self.set(AgentState::Done, now);
            }
            _ => {}
        }
        if self.state == AgentState::Working
            && self.by_subprocs
            && !obs.subprocs_busy
            && self.active_subagents == 0
            && obs.title_busy != Some(true)
            && self.quiet_for(now) >= QUIET_DONE
        {
            return self.set(AgentState::Done, now);
        }
        // The title speaks for programs that set one; hooks speak for hooked sessions.
        if self.lifecycle_hooks || obs.title_busy == Some(true) {
            return None;
        }
        if obs.title_busy.is_some() && !self.output_is_work(now) {
            return None;
        }
        match self.state {
            AgentState::Idle | AgentState::Done | AgentState::NeedsInput if self.output_is_work(now) => {
                let t = self.set(AgentState::Working, now);
                self.by_activity = true;
                t
            }
            AgentState::Working if self.by_activity && !obs.subprocs_busy && self.active_subagents == 0 && self.quiet_for(now) >= QUIET_DONE => {
                self.set(AgentState::Done, now)
            }
            _ => None,
        }
    }

    fn quiet_for(&self, now: Instant) -> Duration {
        self.prev_output.map_or(Duration::MAX, |t| now.saturating_duration_since(t))
    }

    fn output_is_work(&self, now: Instant) -> bool {
        let (Some(start), Some(last)) = (self.burst_start, self.prev_output) else { return false };
        let typing = self.last_input.is_some_and(|i| now.saturating_duration_since(i) < INPUT_QUIET || i >= start);
        !typing && self.quiet_for(now) < BURST_GAP && last.saturating_duration_since(start) >= BURST_MIN
    }

    /// How soon `observe` should run again even if nothing is printed (a silence can
    /// end a turn). `None` when only new output or events can change anything.
    pub fn recheck_in(&self, now: Instant) -> Option<Duration> {
        if self.lifecycle_hooks && !self.by_subprocs {
            return None;
        }
        match self.state {
            AgentState::Working if (self.by_activity || self.by_subprocs) && !self.subprocs_busy => {
                Some(QUIET_DONE.saturating_sub(self.quiet_for(now)).max(Duration::from_millis(100)))
            }
            AgentState::Idle if self.quiet_for(now) < BURST_GAP => Some(Duration::from_millis(250)),
            _ => None,
        }
    }
}

/// Reads a busy / idle hint from a window title. Claude Code prefixes its title with a
/// braille spinner frame while it works and with `✳` when it waits; other CLIs that
/// animate a braille spinner count as busy too.
pub fn title_activity(kind: Option<AgentKind>, title: &str) -> Option<bool> {
    let first = title.trim_start().chars().next()?;
    if ('\u{2801}'..='\u{28FF}').contains(&first) {
        return Some(true);
    }
    (kind == Some(AgentKind::Claude) && first == '✳').then_some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Streams output every 100 ms over `[from, to)` and observes each step.
    fn stream(t: &mut Tracker, start: Instant, from: u64, to: u64) -> Vec<Transition> {
        let mut out = Vec::new();
        let mut at = from;
        while at < to {
            let now = start + ms(at);
            out.extend(t.observe(Observation { title_busy: None, last_output: Some(now), subprocs_busy: false }, now));
            at += 100;
        }
        out
    }

    fn quiet(t: &mut Tracker, start: Instant, at: u64) -> Option<Transition> {
        let last = t.prev_output;
        t.observe(Observation { title_busy: None, last_output: last, subprocs_busy: false }, start + ms(at))
    }

    #[test]
    fn codex_completion_does_not_disable_tracking_the_next_turn() {
        let start = Instant::now();
        let mut t = Tracker::new(start);
        stream(&mut t, start, 0, 1500);
        assert_eq!(t.state, AgentState::Working);
        t.apply(Event::Hook { kind: HookKind::TurnComplete, message: Some("First answer".into()) }, start + ms(1600));
        assert!(t.hooked);
        assert_eq!(t.state, AgentState::Done);
        t.apply(Event::Input, start + ms(4000));
        assert_eq!(t.state, AgentState::Idle);
        assert_eq!(stream(&mut t, start, 6000, 7500), vec![(AgentState::Idle, AgentState::Working)]);
        t.apply(Event::Hook { kind: HookKind::TurnComplete, message: Some("Second answer".into()) }, start + ms(7600));
        assert_eq!(t.state, AgentState::Done);
        assert_eq!(t.message.as_deref(), Some("Second answer"));
        t.apply(Event::Notify("Approval needed: allow command?".into()), start + ms(9000));
        assert_eq!(t.state, AgentState::NeedsPermission);
    }

    #[test]
    fn sustained_output_is_work_and_silence_finishes_it() {
        let t0 = Instant::now();
        let mut t = Tracker::new(t0);
        let changes = stream(&mut t, t0, 0, 1500);
        assert_eq!(changes, vec![(AgentState::Idle, AgentState::Working)]);
        assert_eq!(quiet(&mut t, t0, 3000), None, "a short pause is still work");
        assert!(t.recheck_in(t0 + ms(3000)).is_some());
        assert_eq!(quiet(&mut t, t0, 4500), Some((AgentState::Working, AgentState::Done)));
        assert_eq!(t.recheck_in(t0 + ms(4500)), None);
    }

    #[test]
    fn short_bursts_and_typing_echo_are_not_work() {
        let t0 = Instant::now();
        let mut t = Tracker::new(t0);
        assert!(stream(&mut t, t0, 0, 600).is_empty(), "a redraw is not work");
        // The user types for two seconds: echo is not the agent working.
        let mut at = 5000;
        while at < 7000 {
            t.apply(Event::Input, t0 + ms(at));
            assert!(stream(&mut t, t0, at, at + 100).is_empty());
            at += 100;
        }
        assert_eq!(t.state, AgentState::Idle);
    }

    #[test]
    fn claude_title_spinner_drives_working_and_done() {
        let t0 = Instant::now();
        let mut t = Tracker::new(t0);
        let busy = |b| Observation { title_busy: Some(b), last_output: Some(t0), subprocs_busy: false };
        assert_eq!(t.observe(busy(false), t0), None);
        assert_eq!(t.observe(busy(true), t0 + ms(10)), Some((AgentState::Idle, AgentState::Working)));
        assert_eq!(t.observe(busy(true), t0 + ms(20)), None);
        assert_eq!(t.observe(busy(false), t0 + ms(30)), Some((AgentState::Working, AgentState::Done)));
        // If spinner resumes after Done, it returns to Working!
        assert_eq!(t.observe(busy(true), t0 + ms(40)), Some((AgentState::Done, AgentState::Working)));
        assert_eq!(title_activity(Some(AgentKind::Claude), "⠐ Fix the build"), Some(true));
        assert_eq!(title_activity(Some(AgentKind::Claude), "✳ Fix the build"), Some(false));
        assert_eq!(title_activity(Some(AgentKind::Codex), "✳ x"), None);
        assert_eq!(title_activity(None, "pwsh"), None);
        assert_eq!(title_activity(None, ""), None);
    }

    #[test]
    fn subagents_and_running_scripts_keep_claude_working() {
        let t0 = Instant::now();
        let mut t = Tracker::new(t0);
        let hook = |kind| Event::Hook { kind, message: None };
        t.apply(hook(HookKind::PromptSubmit), t0);
        assert_eq!(t.state, AgentState::Working);

        // Title reverting to `✳` (Some(false)) or a child script ringing Bell while hooked
        // or while subprocs_busy is true must NOT end the Working state.
        let obs_script = Observation { title_busy: Some(false), last_output: Some(t0), subprocs_busy: true };
        assert_eq!(t.observe(obs_script, t0 + ms(2000)), None);
        assert_eq!(t.apply(Event::Bell, t0 + ms(2500)), None);
        assert_eq!(t.apply(hook(HookKind::Notification { permission: false }), t0 + ms(3000)), None);
        assert_eq!(t.state, AgentState::Working);

        // Even if Stop arrives while a background subagent/script is still running, it stays Working.
        assert_eq!(t.apply(hook(HookKind::Stop), t0 + ms(4000)), None);
        assert_eq!(t.state, AgentState::Working);

        // Once the child script/subagent finishes and QUIET_DONE elapses, it settles to Done.
        let obs_idle = Observation { title_busy: Some(false), last_output: Some(t0 + ms(4000)), subprocs_busy: false };
        assert_eq!(t.observe(obs_idle, t0 + ms(4500)), None);
        assert_eq!(t.observe(obs_idle, t0 + ms(8000)), Some((AgentState::Working, AgentState::Done)));
    }

    #[test]
    fn hooks_are_authoritative() {
        let t0 = Instant::now();
        let mut t = Tracker::new(t0);
        let hook = |kind, message: Option<&str>| Event::Hook { kind, message: message.map(String::from) };
        assert_eq!(t.apply(hook(HookKind::SessionStart, None), t0), None);
        assert!(t.hooked);
        assert_eq!(t.apply(hook(HookKind::PromptSubmit, Some("fix it")), t0), Some((AgentState::Idle, AgentState::Working)));
        assert_eq!(t.message.as_deref(), Some("fix it"));
        // Activity heuristics are off once hooked: silence does not end the turn.
        stream(&mut t, t0, 0, 1500);
        assert_eq!(quiet(&mut t, t0, 9000), None);
        let perm = hook(HookKind::Notification { permission: true }, Some("Claude needs your permission to use Bash"));
        assert_eq!(t.apply(perm, t0 + ms(9000)), Some((AgentState::Working, AgentState::NeedsPermission)));
        assert_eq!(t.since, t0 + ms(9000));
        assert_eq!(t.apply(Event::Input, t0 + ms(9500)), Some((AgentState::NeedsPermission, AgentState::Idle)));
        assert_eq!(t.apply(hook(HookKind::SubagentStop, None), t0 + ms(9600)), None);
        assert_eq!(t.apply(hook(HookKind::Stop, None), t0 + ms(9700)), Some((AgentState::Idle, AgentState::Done)));
        assert_eq!(t.apply(hook(HookKind::SessionEnd, None), t0 + ms(9800)), Some((AgentState::Done, AgentState::Idle)));
        assert!(!t.hooked);
    }

    #[test]
    fn bells_notifications_and_exits() {
        let t0 = Instant::now();
        let mut t = Tracker::new(t0);
        assert_eq!(t.apply(Event::Bell, t0), Some((AgentState::Idle, AgentState::NeedsInput)));
        assert_eq!(t.apply(Event::Bell, t0), None);
        assert_eq!(t.apply(Event::Notify("Claude needs your permission to use Edit".into()), t0), Some((AgentState::NeedsInput, AgentState::NeedsPermission)));
        assert_eq!(t.apply(Event::Notify("Task complete".into()), t0), Some((AgentState::NeedsPermission, AgentState::Done)));
        assert_eq!(t.apply(Event::Notify("Claude is waiting for your input".into()), t0), Some((AgentState::Done, AgentState::NeedsInput)));
        assert_eq!(t.apply(Event::Exited(Some(1)), t0), Some((AgentState::NeedsInput, AgentState::Error)));
        assert_eq!(t.apply(Event::Exited(Some(0)), t0), Some((AgentState::Error, AgentState::Done)));
    }

    #[test]
    fn hook_names_parse() {
        assert_eq!(HookKind::parse("UserPromptSubmit", None, None), Some(HookKind::PromptSubmit));
        assert_eq!(HookKind::parse("Notification", Some("permission_prompt"), None), Some(HookKind::Notification { permission: true }));
        assert_eq!(HookKind::parse("Notification", Some("idle_prompt"), Some("permission")), Some(HookKind::Notification { permission: false }));
        assert_eq!(HookKind::parse("Notification", None, Some("Claude needs your permission")), Some(HookKind::Notification { permission: true }));
        assert_eq!(HookKind::parse("agent-turn-complete", None, None), Some(HookKind::TurnComplete));
        assert_eq!(HookKind::parse("PreToolUse", None, None), None);
    }

    #[test]
    fn acknowledging_clears_attention_only() {
        let t0 = Instant::now();
        let mut t = Tracker::new(t0);
        t.apply(Event::Hook { kind: HookKind::PromptSubmit, message: None }, t0);
        assert_eq!(t.acknowledge(t0), None, "work is not dismissed");
        t.apply(Event::Hook { kind: HookKind::Stop, message: None }, t0);
        assert_eq!(t.acknowledge(t0), Some((AgentState::Done, AgentState::Idle)));
    }

    #[test]
    fn urgency_orders_attention_first() {
        let mut v = [AgentState::Idle, AgentState::Done, AgentState::Working, AgentState::NeedsPermission, AgentState::NeedsInput];
        v.sort_by_key(|s| s.urgency());
        assert_eq!(v[0], AgentState::NeedsPermission);
        assert_eq!(v[4], AgentState::Idle);
        assert!(AgentState::Done.needs_attention() && !AgentState::Working.needs_attention());
    }
}
