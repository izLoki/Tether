//! Command palette entries and the fuzzy matcher that ranks them.
//!
//! The app fills a [`Registry`] each time the palette opens (shortcut actions, shells,
//! hosts, themes, open tabs...). Other features add their own entries by registering a
//! provider once with [`register_provider`]; a provider's entries usually carry a
//! [`Command::Run`] closure, so the feature needs no change in the app to act on them.

use std::sync::{Arc, Mutex};

use eframe::egui;
use uuid::Uuid;

use crate::keybindings::Action;
use crate::layout::{BarOrientation, Container, Side, TabPosition, View};

/// What choosing an entry does. The app interprets every variant but `Run`.
#[derive(Clone)]
pub enum Command {
    /// A rebindable shortcut action (the same code path as its key).
    App(Action),
    /// Open a local shell by `pty::shells` id.
    NewShell(String),
    /// Open a terminal to a saved host.
    Connect(Uuid),
    /// Open an SFTP pane to a saved host.
    ConnectSftp(Uuid),
    SetTheme(String),
    SetTabPosition(TabPosition),
    SetSidebarSide(Side),
    /// Show a view (un-hiding it, opening its container).
    ShowView(View),
    MoveView(View, Container),
    SetBar(Container, BarOrientation),
    ToggleDimPanes,
    ToggleFocusFollowsMouse,
    /// Focus an open tab by uid.
    FocusTab(u64),
    RestoreWorkspace(String),
    OpenLayoutSettings,
    /// The "Import hosts" dialog (Bitvise, SSH config).
    ImportHosts,
    /// Choose a local project and shell, then start an AI agent CLI.
    LaunchAgent {
        kind: crate::agents::AgentKind,
        command: &'static str,
    },
    /// Anything else: runs on the UI thread when chosen.
    Run(Arc<dyn Fn(&egui::Context) + Send + Sync>),
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::Run(_) => f.write_str("Run(..)"),
            Command::App(a) => write!(f, "App({a:?})"),
            Command::NewShell(s) => write!(f, "NewShell({s})"),
            Command::Connect(id) => write!(f, "Connect({id})"),
            Command::ConnectSftp(id) => write!(f, "ConnectSftp({id})"),
            Command::SetTheme(t) => write!(f, "SetTheme({t})"),
            Command::SetTabPosition(p) => write!(f, "SetTabPosition({p:?})"),
            Command::SetSidebarSide(s) => write!(f, "SetSidebarSide({s:?})"),
            Command::ShowView(v) => write!(f, "ShowView({v:?})"),
            Command::MoveView(v, c) => write!(f, "MoveView({v:?}, {c:?})"),
            Command::SetBar(c, b) => write!(f, "SetBar({c:?}, {b:?})"),
            Command::ToggleDimPanes => f.write_str("ToggleDimPanes"),
            Command::ToggleFocusFollowsMouse => f.write_str("ToggleFocusFollowsMouse"),
            Command::FocusTab(uid) => write!(f, "FocusTab({uid})"),
            Command::RestoreWorkspace(n) => write!(f, "RestoreWorkspace({n})"),
            Command::OpenLayoutSettings => f.write_str("OpenLayoutSettings"),
            Command::ImportHosts => f.write_str("ImportHosts"),
            Command::LaunchAgent { command, .. } => write!(f, "LaunchAgent({command})"),
        }
    }
}

/// One palette row.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Short group label shown on the left ("Tab", "Host", "Theme"...).
    pub category: &'static str,
    pub title: String,
    /// Dim text after the title (an address, a path).
    pub detail: String,
    /// Shortcut hint shown on the right, e.g. `Ctrl+Shift+D`.
    pub keys: Option<String>,
    /// Extra words that find the entry without being shown ("zoom" for maximize).
    pub keywords: String,
    pub command: Command,
}

impl Entry {
    pub fn new(category: &'static str, title: impl Into<String>, command: Command) -> Self {
        Self { category, title: title.into(), detail: String::new(), keys: None, keywords: String::new(), command }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    pub fn keys(mut self, keys: Option<String>) -> Self {
        self.keys = keys.filter(|k| !k.is_empty());
        self
    }

    /// An entry for a shortcut action, with its current binding as the hint.
    pub fn action(category: &'static str, action: Action) -> Self {
        let mut e = Self::new(category, action.title(), Command::App(action)).keys(crate::keybindings::binding(action).map(|b| b.label()));
        e.keywords = action_keywords(action).to_string();
        e
    }

    /// The text the matcher searches: category, title and detail.
    fn haystack(&self) -> String {
        format!("{} {} {}", self.title, self.category, self.keywords)
    }
}

/// A ranked search result: the entry index, its score and the matched title chars.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub index: usize,
    pub score: i32,
    /// Char indices (into `title`) that matched, for highlighting.
    pub title_matches: Vec<usize>,
}

#[derive(Default, Clone)]
pub struct Registry {
    entries: Vec<Entry>,
}

impl Registry {
    pub fn add(&mut self, entry: Entry) {
        self.entries.push(entry);
    }

    pub fn extend(&mut self, entries: impl IntoIterator<Item = Entry>) {
        self.entries.extend(entries);
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries matching `query`, best first. An empty query keeps registration order.
    pub fn search(&self, query: &str) -> Vec<Hit> {
        let q = query.trim();
        if q.is_empty() {
            return (0..self.entries.len()).map(|index| Hit { index, score: 0, title_matches: Vec::new() }).collect();
        }
        let mut hits: Vec<Hit> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, e)| {
                // Every whitespace-separated word must match (in title, category or detail).
                let mut score = 0;
                let mut title_matches = Vec::new();
                for word in q.split_whitespace() {
                    if let Some((s, m)) = fuzzy_match(word, &e.title) {
                        score += s + 20;
                        title_matches.extend(m);
                    } else if let Some((s, _)) = fuzzy_match(word, &e.haystack()) {
                        score += s;
                    } else if let Some((s, _)) = fuzzy_match(word, &e.detail) {
                        score += s / 2;
                    } else {
                        return None;
                    }
                }
                title_matches.sort_unstable();
                title_matches.dedup();
                Some(Hit { index, score, title_matches })
            })
            .collect();
        // Stable: ties keep registration order.
        hits.sort_by_key(|h| std::cmp::Reverse(h.score));
        hits
    }

    /// Runs every registered provider into this registry.
    pub fn add_providers(&mut self) {
        let providers = PROVIDERS.lock().map(|p| p.clone()).unwrap_or_default();
        for p in providers {
            p(self);
        }
    }
}

/// Synonyms people type for some actions.
fn action_keywords(a: Action) -> &'static str {
    match a {
        Action::ZoomPane => "zoom fullscreen pane",
        Action::ToggleSidebar | Action::ToggleSecondarySidebar => "sidebar panel hide show",
        Action::TogglePanel => "bottom hide show",
        Action::ResetLayout => "default views sidebar panel",
        Action::SplitRight => "pane vertical",
        Action::SplitDown => "pane horizontal",
        Action::CycleTabPosition => "vertical tabs sidebar",
        Action::FocusPaneLeft | Action::FocusPaneRight | Action::FocusPaneUp | Action::FocusPaneDown => "navigate move switch",
        Action::NewLocalTerminal => "shell console",
        Action::ToggleBroadcast => "all terminals sync",
        Action::NextAttention => "agent waiting jump",
        Action::ToggleAgents => "ai claude",
        Action::ToggleCodex => "ai codex openai",
        _ => "",
    }
}

pub type Provider = fn(&mut Registry);

static PROVIDERS: Mutex<Vec<Provider>> = Mutex::new(Vec::new());

/// Adds `provider` to every palette opened from now on (registering twice is a no-op).
pub fn register_provider(provider: Provider) {
    if let Ok(mut list) = PROVIDERS.lock()
        && !list.iter().any(|p| std::ptr::fn_addr_eq(*p, provider))
    {
        list.push(provider);
    }
}

/// Subsequence match of `needle` in `hay`, case-insensitive. Returns a score (higher is
/// better) and the char indices of `hay` that matched. Rewards matches at the start,
/// at word starts (after a space, `-`, `_`, `/`, `.` or a lower→upper change) and runs
/// of consecutive characters; gaps cost a little.
pub fn fuzzy_match(needle: &str, hay: &str) -> Option<(i32, Vec<usize>)> {
    let n: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    if n.is_empty() {
        return Some((0, Vec::new()));
    }
    let h: Vec<char> = hay.chars().collect();
    let lower: Vec<char> = h.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    // Exact substring beats any scattered match.
    let needle_str: String = n.iter().collect();
    let lower_str: String = lower.iter().collect();
    let substring_at = lower_str.find(&needle_str).map(|byte| lower_str[..byte].chars().count());

    let word_start =
        |i: usize| i == 0 || matches!(h[i - 1], ' ' | '-' | '_' | '/' | '\\' | '.' | ':' | '·' | '(') || (h[i - 1].is_lowercase() && h[i].is_uppercase());

    // Greedy forward pass, preferring word starts: for each needle char take the next
    // word-start occurrence if one comes before the next plain occurrence's run ends.
    let mut positions = Vec::with_capacity(n.len());
    let mut from = 0;
    for &c in &n {
        let next = (from..lower.len()).find(|&i| lower[i] == c)?;
        // A word-start match later on is better than a mid-word one, unless the
        // plain match continues a run.
        let continues_run = positions.last().is_some_and(|&p: &usize| p + 1 == next);
        let chosen = if continues_run || word_start(next) { next } else { (next..lower.len()).find(|&i| lower[i] == c && word_start(i)).unwrap_or(next) };
        positions.push(chosen);
        from = chosen + 1;
    }
    if let Some(at) = substring_at {
        let run: Vec<usize> = (at..at + n.len()).collect();
        let score = score_positions(&run, &word_start, h.len()) + 30;
        let scattered = score_positions(&positions, &word_start, h.len());
        return Some(if score >= scattered { (score, run) } else { (scattered, positions) });
    }
    Some((score_positions(&positions, &word_start, h.len()), positions))
}

fn score_positions(pos: &[usize], word_start: &dyn Fn(usize) -> bool, len: usize) -> i32 {
    let mut score = 0i32;
    for (k, &p) in pos.iter().enumerate() {
        score += 10;
        if word_start(p) {
            score += 15;
        }
        if p == 0 {
            score += 12;
        }
        if k > 0 {
            let gap = p - pos[k - 1] - 1;
            if gap == 0 {
                score += 12;
            } else {
                score -= (gap as i32).min(8);
            }
        }
    }
    // Shorter haystacks rank a little higher for the same match.
    score - (len as i32 / 12)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(r: &Registry, q: &str) -> Vec<String> {
        r.search(q).iter().map(|h| r.entries()[h.index].title.clone()).collect()
    }

    #[test]
    fn fuzzy_requires_a_subsequence() {
        assert!(fuzzy_match("spr", "Split right").is_some());
        assert!(fuzzy_match("xyz", "Split right").is_none());
        assert!(fuzzy_match("rs", "Split right").is_none(), "order matters");
        assert_eq!(fuzzy_match("", "anything").map(|m| m.0), Some(0));
    }

    #[test]
    fn fuzzy_is_case_insensitive_and_reports_positions() {
        let (_, pos) = fuzzy_match("SR", "split right").unwrap();
        assert_eq!(pos, vec![0, 6], "prefers word starts");
        let (_, pos) = fuzzy_match("ight", "Split right").unwrap();
        assert_eq!(pos, vec![7, 8, 9, 10], "substring run");
    }

    #[test]
    fn fuzzy_ranks_word_starts_and_runs_higher() {
        let start = fuzzy_match("ter", "Terminal").unwrap().0;
        let middle = fuzzy_match("ter", "Shutter").unwrap().0;
        assert!(start > middle, "{start} vs {middle}");
        let acronym = fuzzy_match("nlt", "New local terminal").unwrap().0;
        let scattered = fuzzy_match("nlt", "Unlimited").unwrap().0;
        assert!(acronym > scattered, "{acronym} vs {scattered}");
    }

    #[test]
    fn registry_search_ranks_and_filters() {
        let mut r = Registry::default();
        r.add(Entry::new("Action", "Toggle sidebar", Command::ShowView(View::Sessions)));
        r.add(Entry::new("Theme", "Nord", Command::SetTheme("Nord".into())));
        r.add(Entry::new("Action", "Split right", Command::App(Action::SplitRight)));
        r.add(Entry::new("Host", "api-prod-01", Command::Connect(Uuid::nil())).detail("deploy@10.0.4.21"));
        assert_eq!(r.len(), 4);
        assert_eq!(titles(&r, ""), vec!["Toggle sidebar", "Nord", "Split right", "api-prod-01"], "empty query keeps order");
        assert_eq!(titles(&r, "split")[0], "Split right");
        assert_eq!(titles(&r, "theme nord"), vec!["Nord"], "every word must match; category counts");
        assert_eq!(titles(&r, "10.0.4"), vec!["api-prod-01"], "detail is searched");
        assert!(titles(&r, "zzz").is_empty());
        let hit = &r.search("sid")[0];
        assert_eq!(r.entries()[hit.index].title, "Toggle sidebar");
        assert_eq!(hit.title_matches, vec![7, 8, 9]);
    }

    fn sample_provider(r: &mut Registry) {
        r.add(Entry::new("Agent", "Show agents", Command::Run(Arc::new(|_| {}))));
    }

    #[test]
    fn providers_add_entries_once() {
        register_provider(sample_provider);
        register_provider(sample_provider);
        let mut r = Registry::default();
        r.add_providers();
        assert_eq!(r.entries().iter().filter(|e| e.title == "Show agents").count(), 1);
        assert!(matches!(r.entries()[0].command, Command::Run(_)));
    }

    #[test]
    fn action_entries_carry_their_shortcut() {
        let e = Entry::action("Pane", Action::SplitRight);
        assert_eq!(e.title, Action::SplitRight.title());
        assert_eq!(e.keys.as_deref(), Some("Ctrl+Shift+D"));
        assert!(Entry::action("Pane", Action::MoveSidebar).keys.is_none());
        let mut r = Registry::default();
        r.add(Entry::action("Pane", Action::ZoomPane));
        assert_eq!(r.search("zoom").len(), 1, "keywords are searched");
    }
}
