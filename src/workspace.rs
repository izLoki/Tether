//! Saved layouts: the split tree with each tab reduced to (profile, kind), so a whole
//! arrangement of terminals and SFTP panes can be reopened in one click.
//!
//! We keep our own tree format instead of serializing `egui_dock::DockState`: its
//! cached rects hold infinities that JSON cannot represent, and our format stays
//! stable across egui_dock upgrades.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use egui_dock::{DockState, Node, NodeIndex, Tree};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::profile::write_json_atomic;

pub const LAST_SESSION: &str = "Last session";

/// Serialized as `"Terminal"`, `"Sftp"` or `{"Local":{"shell_id":"pwsh"}}`, so layouts
/// saved before local terminals existed still load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum TabKind {
    Terminal,
    Sftp,
    /// A local shell (`pty::shells`); dropped on restore if that shell is gone.
    Local {
        shell_id: String,
    },
    /// A kind this version doesn't know (saved by a newer tether): skipped on restore
    /// instead of failing the whole workspace file. Never saved.
    Unknown,
}

impl<'de> Deserialize<'de> for TabKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        enum Known {
            Terminal,
            Sftp,
            Local { shell_id: String },
        }
        let value = serde_json::Value::deserialize(d)?;
        Ok(match serde_json::from_value(value) {
            Ok(Known::Terminal) => TabKind::Terminal,
            Ok(Known::Sftp) => TabKind::Sftp,
            Ok(Known::Local { shell_id }) => TabKind::Local { shell_id },
            Err(_) => TabKind::Unknown,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabSpec {
    /// The host; nil for local shells.
    #[serde(default)]
    pub profile_id: Uuid,
    pub kind: TabKind,
    /// A name the user gave the tab (renamed in the tab list).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Working directory of the terminal when saved, restored on session restore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// True when an SSH terminal tab has a companion SFTP pane linked to its right side.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub with_sftp: bool,
    /// Local working directory of the SFTP pane (standalone or linked).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sftp_local_cwd: Option<String>,
    /// Whether the Local pane was visible in this SFTP view when saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sftp_show_local: Option<bool>,
}

impl TabSpec {
    pub fn new(profile_id: Uuid, kind: TabKind) -> Self {
        Self { profile_id, kind, title: None, cwd: None, with_sftp: false, sftp_local_cwd: None, sftp_show_local: None }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Layout<T> {
    Leaf(Vec<T>),
    /// `side_by_side` = children left|right, otherwise top/bottom.
    Split {
        side_by_side: bool,
        fraction: f32,
        first: Box<Layout<T>>,
        second: Box<Layout<T>>,
    },
}

impl<T> Layout<T> {
    /// Every tab, in tree order.
    pub fn tabs(&self) -> Vec<&T> {
        match self {
            Layout::Leaf(tabs) => tabs.iter().collect(),
            Layout::Split { first, second, .. } => {
                let mut out = first.tabs();
                out.extend(second.tabs());
                out
            }
        }
    }
}

/// Captures the main surface of a dock as a plain tree.
pub fn capture<T>(dock: &DockState<T>, spec: impl Fn(&T) -> Option<TabSpec> + Copy) -> Option<Layout<TabSpec>> {
    let tree = dock.main_surface();
    if tree.is_empty() {
        return None;
    }
    capture_node(tree, NodeIndex::root(), spec)
}

fn capture_node<T>(tree: &Tree<T>, idx: NodeIndex, spec: impl Fn(&T) -> Option<TabSpec> + Copy) -> Option<Layout<TabSpec>> {
    match &tree[idx] {
        Node::Empty => None,
        Node::Leaf(leaf) => {
            let tabs: Vec<TabSpec> = leaf.tabs().iter().filter_map(spec).collect();
            (!tabs.is_empty()).then_some(Layout::Leaf(tabs))
        }
        Node::Horizontal(s) | Node::Vertical(s) => {
            let side_by_side = matches!(tree[idx], Node::Horizontal(_));
            let first = capture_node(tree, idx.left(), spec);
            let second = capture_node(tree, idx.right(), spec);
            match (first, second) {
                (Some(a), Some(b)) => Some(Layout::Split { side_by_side, fraction: s.fraction, first: Box::new(a), second: Box::new(b) }),
                (a, b) => a.or(b),
            }
        }
    }
}

/// Rebuilds a dock, dropping tabs `make` rejects and collapsing emptied splits.
pub fn rebuild<T>(layout: &Layout<TabSpec>, make: &mut impl FnMut(&TabSpec) -> Option<T>) -> DockState<T> {
    let Some(mut built) = prune(layout, make) else { return DockState::new(Vec::new()) };
    let first = take_first_leaf(&mut built);
    let mut dock = DockState::new(first);
    place(dock.main_surface_mut(), NodeIndex::root(), built);
    dock
}

fn prune<T>(layout: &Layout<TabSpec>, make: &mut impl FnMut(&TabSpec) -> Option<T>) -> Option<Layout<T>> {
    match layout {
        Layout::Leaf(specs) => {
            let tabs: Vec<T> = specs.iter().filter_map(&mut *make).collect();
            (!tabs.is_empty()).then_some(Layout::Leaf(tabs))
        }
        Layout::Split { side_by_side, fraction, first, second } => match (prune(first, make), prune(second, make)) {
            (Some(a), Some(b)) => Some(Layout::Split { side_by_side: *side_by_side, fraction: *fraction, first: Box::new(a), second: Box::new(b) }),
            (a, b) => a.or(b),
        },
    }
}

fn take_first_leaf<T>(layout: &mut Layout<T>) -> Vec<T> {
    match layout {
        Layout::Leaf(tabs) => std::mem::take(tabs),
        Layout::Split { first, .. } => take_first_leaf(first),
    }
}

/// `idx` already holds the tabs of `layout`'s first leaf; splitting moves them into the
/// first child and opens the second child with the second subtree's first leaf.
fn place<T>(tree: &mut Tree<T>, idx: NodeIndex, layout: Layout<T>) {
    if let Layout::Split { side_by_side, fraction, first, mut second } = layout {
        let tabs = take_first_leaf(&mut second);
        let [a, b] = if side_by_side { tree.split_right(idx, fraction, tabs) } else { tree.split_below(idx, fraction, tabs) };
        place(tree, a, *first);
        place(tree, b, *second);
    }
}

pub struct Workspaces {
    path: PathBuf,
    map: BTreeMap<String, Layout<TabSpec>>,
}

impl Workspaces {
    pub fn load(path: PathBuf) -> Self {
        let map = crate::json_file::load_or_default(&path);
        Self { path, map }
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.map.keys().filter(|k| *k != LAST_SESSION).cloned().collect();
        if self.map.contains_key(LAST_SESSION) {
            names.insert(0, LAST_SESSION.to_string());
        }
        names
    }

    pub fn get(&self, name: &str) -> Option<&Layout<TabSpec>> {
        self.map.get(name)
    }

    /// Stores a layout; `None` (nothing open) removes the entry.
    pub fn put(&mut self, name: String, layout: Option<Layout<TabSpec>>) -> Result<()> {
        match layout {
            Some(l) => self.map.insert(name, l),
            None => self.map.remove(&name),
        };
        write_json_atomic(&self.path, &self.map)
    }

    /// Renames a saved workspace. "Last session" is managed by the app and can't be a
    /// source or target; an existing name is never overwritten.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        let to = to.trim();
        if from == LAST_SESSION || to == LAST_SESSION {
            bail!("“{LAST_SESSION}” is kept automatically and can't be renamed");
        }
        if to.is_empty() {
            bail!("The name can't be empty");
        }
        if from == to {
            return Ok(());
        }
        if self.map.contains_key(to) {
            bail!("A workspace named “{to}” already exists");
        }
        let layout = self.map.remove(from).ok_or_else(|| anyhow!("Workspace “{from}” no longer exists"))?;
        self.map.insert(to.to_string(), layout);
        write_json_atomic(&self.path, &self.map)
    }

    pub fn remove(&mut self, name: &str) -> Result<()> {
        self.map.remove(name);
        write_json_atomic(&self.path, &self.map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: Uuid, kind: TabKind) -> TabSpec {
        TabSpec::new(id, kind)
    }

    fn sample() -> (Uuid, DockState<TabSpec>) {
        let id = Uuid::new_v4();
        let mut dock = DockState::new(vec![spec(id, TabKind::Terminal), spec(id, TabKind::Terminal)]);
        let [left, _] = dock.main_surface_mut().split_right(NodeIndex::root(), 0.6, vec![spec(id, TabKind::Sftp)]);
        dock.main_surface_mut().split_below(left, 0.3, vec![spec(id, TabKind::Terminal)]);
        (id, dock)
    }

    #[test]
    fn capture_rebuild_roundtrip_preserves_tree() {
        let (_, dock) = sample();
        let layout = capture(&dock, |t| Some(t.clone())).unwrap();
        let rebuilt = rebuild(&layout, &mut |s| Some(s.clone()));
        assert_eq!(capture(&rebuilt, |t| Some(t.clone())).unwrap(), layout);
        assert_eq!(rebuilt.iter_all_tabs().count(), 4);
    }

    #[test]
    fn rebuild_drops_missing_tabs_and_collapses_splits() {
        let (_, dock) = sample();
        let layout = capture(&dock, |t| Some(t.clone())).unwrap();
        let rebuilt = rebuild(&layout, &mut |s| (s.kind == TabKind::Terminal).then(|| s.clone()));
        assert_eq!(rebuilt.iter_all_tabs().count(), 3);
        assert!(matches!(capture(&rebuilt, |t| Some(t.clone())).unwrap(), Layout::Split { side_by_side: false, .. }));
    }

    #[test]
    fn persists_to_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws.json");
        let (_, dock) = sample();
        let mut ws = Workspaces::load(path.clone());
        ws.put("dev".into(), capture(&dock, |t| Some(t.clone()))).unwrap();
        ws.put(LAST_SESSION.into(), capture(&dock, |t| Some(t.clone()))).unwrap();
        let ws = Workspaces::load(path);
        assert_eq!(ws.names(), vec![LAST_SESSION.to_string(), "dev".to_string()]);
        assert_eq!(ws.get("dev"), capture(&dock, |t| Some(t.clone())).as_ref());
    }

    #[test]
    fn rename_moves_the_layout_and_refuses_clashes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws.json");
        let (_, dock) = sample();
        let mut ws = Workspaces::load(path.clone());
        ws.put("dev".into(), capture(&dock, |t| Some(t.clone()))).unwrap();
        ws.put("prod".into(), capture(&dock, |t| Some(t.clone()))).unwrap();
        ws.put(LAST_SESSION.into(), capture(&dock, |t| Some(t.clone()))).unwrap();
        assert!(ws.rename("dev", "prod").is_err(), "never overwrites");
        assert!(ws.rename("dev", "  ").is_err());
        assert!(ws.rename(LAST_SESSION, "kept").is_err());
        assert!(ws.rename("dev", LAST_SESSION).is_err());
        ws.rename("dev", " staging ").unwrap();
        let ws = Workspaces::load(path);
        assert_eq!(ws.names(), vec![LAST_SESSION.to_string(), "prod".to_string(), "staging".to_string()]);
    }

    #[test]
    fn layout_lists_every_tab_in_order() {
        let (_, dock) = sample();
        let layout = capture(&dock, |t| Some(t.clone())).unwrap();
        let kinds: Vec<TabKind> = layout.tabs().iter().map(|t| t.kind.clone()).collect();
        assert_eq!(kinds, vec![TabKind::Terminal, TabKind::Terminal, TabKind::Terminal, TabKind::Sftp]);
    }

    #[test]
    fn tab_kinds_stay_compatible_with_old_layouts() {
        let id = Uuid::new_v4();
        let old = format!(r#"{{"Leaf":[{{"profile_id":"{id}","kind":"Terminal"}},{{"profile_id":"{id}","kind":"Sftp"}}]}}"#);
        let layout: Layout<TabSpec> = serde_json::from_str(&old).unwrap();
        assert_eq!(layout, Layout::Leaf(vec![spec(id, TabKind::Terminal), spec(id, TabKind::Sftp)]));
        let local = Layout::Leaf(vec![spec(Uuid::nil(), TabKind::Local { shell_id: "wsl:Ubuntu".into() })]);
        let text = serde_json::to_string(&local).unwrap();
        assert!(text.contains(r#"{"Local":{"shell_id":"wsl:Ubuntu"}}"#), "{text}");
        assert_eq!(serde_json::from_str::<Layout<TabSpec>>(&text).unwrap(), local);
        let no_profile: TabSpec = serde_json::from_str(r#"{"kind":{"Local":{"shell_id":"cmd"}}}"#).unwrap();
        assert!(no_profile.profile_id.is_nil());
    }

    #[test]
    fn tabs_of_unknown_kinds_do_not_break_the_file() {
        let text = r#"{"Leaf":[{"kind":"Terminal"},{"kind":{"Notebook":{"path":"x"}}},{"kind":"Browser"}]}"#;
        let layout: Layout<TabSpec> = serde_json::from_str(text).unwrap();
        let kinds: Vec<TabKind> = layout.tabs().iter().map(|t| t.kind.clone()).collect();
        assert_eq!(kinds, vec![TabKind::Terminal, TabKind::Unknown, TabKind::Unknown]);
    }

    #[test]
    fn custom_titles_round_trip_and_stay_optional() {
        let titled = TabSpec { title: Some("build box".into()), ..spec(Uuid::nil(), TabKind::Terminal) };
        let text = serde_json::to_string(&titled).unwrap();
        assert!(text.contains("build box"));
        assert_eq!(serde_json::from_str::<TabSpec>(&text).unwrap(), titled);
        let plain = serde_json::to_string(&spec(Uuid::nil(), TabKind::Sftp)).unwrap();
        assert!(!plain.contains("title"), "{plain}");
    }

    #[test]
    fn cwd_round_trips_and_stays_optional() {
        let with_cwd = TabSpec { cwd: Some(r"D:\Projects\Personal\tether".into()), ..spec(Uuid::nil(), TabKind::Local { shell_id: "pwsh".into() }) };
        let text = serde_json::to_string(&with_cwd).unwrap();
        assert!(text.contains(r"D:\\Projects\\Personal\\tether"), "{text}");
        assert_eq!(serde_json::from_str::<TabSpec>(&text).unwrap(), with_cwd);
        let plain = serde_json::to_string(&spec(Uuid::nil(), TabKind::Terminal)).unwrap();
        assert!(!plain.contains("cwd"), "{plain}");
    }

    #[test]
    fn empty_dock_captures_nothing() {
        let dock: DockState<TabSpec> = DockState::new(Vec::new());
        assert!(capture(&dock, |t| Some(t.clone())).is_none());
    }
}
