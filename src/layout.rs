//! Window arrangement settings: where tabs go, which side the primary side bar docks
//! on, where each view lives (see [`crate::views`]) and pane behaviour (dimming, focus
//! on hover).
//!
//! Stored under `settings.layout`; every field has a serde default so settings files
//! written before this existed keep loading, and the single-sidebar settings of older
//! versions are migrated on load (see [`migrate_views`]).

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::views::SIDE_DEFAULT;
pub use crate::views::{BarOrientation, Container, ContainerState, View, ViewLayout};

/// Where the tabs of the workspace are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TabPosition {
    /// Vertical tabs in the Sessions view (wherever it is placed); the panes get tab
    /// strips back while that view is hidden or folded away.
    #[default]
    Sidebar,
    /// A tab strip on top of every pane (egui_dock's own tab bars).
    Top,
}

impl TabPosition {
    pub const ALL: [TabPosition; 2] = [TabPosition::Sidebar, TabPosition::Top];

    pub fn label(self) -> &'static str {
        match self {
            TabPosition::Sidebar => "Sessions view",
            TabPosition::Top => "Top of panes",
        }
    }

    /// The other position (for "cycle tab position").
    pub fn next(self) -> Self {
        match self {
            TabPosition::Sidebar => TabPosition::Top,
            TabPosition::Top => TabPosition::Sidebar,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Side {
    #[default]
    Left,
    Right,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Side::Left => "Left",
            Side::Right => "Right",
        }
    }

    pub fn flip(self) -> Self {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

/// Which edge of the main window the movable mini-dock bar snaps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MiniBarSide {
    #[default]
    Bottom,
    Top,
    Left,
    Right,
}

impl MiniBarSide {
    pub const ALL: [MiniBarSide; 4] = [MiniBarSide::Bottom, MiniBarSide::Top, MiniBarSide::Left, MiniBarSide::Right];

    pub fn is_horizontal(self) -> bool {
        matches!(self, MiniBarSide::Bottom | MiniBarSide::Top)
    }

    pub fn label(self) -> &'static str {
        match self {
            MiniBarSide::Bottom => "Bottom",
            MiniBarSide::Top => "Top",
            MiniBarSide::Left => "Left",
            MiniBarSide::Right => "Right",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "RawLayout")]
pub struct LayoutSettings {
    pub tab_position: TabPosition,
    /// Vertical tabs: group rows under a header per pane (split).
    pub group_by_pane: bool,
    /// Vertical tabs: one line per tab instead of title + subtitle.
    pub compact_tabs: bool,
    /// The side of the primary side bar (the secondary one takes the other).
    pub sidebar_side: Side,
    /// Which view sits in which container, their order, sizes and folded state.
    pub views: ViewLayout,
    /// Fade panes that don't have focus.
    pub dim_inactive_panes: bool,
    /// 0..=1, how much inactive panes are faded.
    pub dim_amount: f32,
    /// Hovering a pane focuses it.
    pub focus_follows_mouse: bool,
    /// Edge where the mini-dock bar for popped-up / minimized windows is docked.
    pub mini_bar_side: MiniBarSide,
    /// 0..=1 offset along the edge where the mini-dock bar sits.
    pub mini_bar_offset: f32,
}

impl LayoutSettings {
    pub const DIM_MAX: f32 = 0.8;

    pub fn dim_amount(&self) -> f32 {
        sane(self.dim_amount, 0.0, Self::DIM_MAX, 0.35)
    }

    /// The window side `c` docks on (`None` for the panel).
    pub fn side_of(&self, c: Container) -> Option<Side> {
        match c {
            Container::Primary => Some(self.sidebar_side),
            Container::Secondary => Some(self.sidebar_side.flip()),
            Container::Panel => None,
        }
    }

    /// The side bar docked on `side`.
    pub fn container_at(&self, side: Side) -> Container {
        if side == self.sidebar_side { Container::Primary } else { Container::Secondary }
    }

    /// Whether the panes hide their tab strips: vertical tabs are on screen instead.
    pub fn tab_bars_hidden(&self) -> bool {
        self.tab_position == TabPosition::Sidebar && self.views.is_shown(View::Sessions)
    }

    /// Back to the default arrangement of views (keeps tab and pane preferences).
    pub fn reset_views(&mut self) {
        self.views = ViewLayout::default();
        self.sidebar_side = Side::Left;
    }
}

impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            tab_position: TabPosition::Sidebar,
            group_by_pane: true,
            compact_tabs: false,
            sidebar_side: Side::Left,
            views: ViewLayout::default(),
            dim_inactive_panes: false,
            dim_amount: 0.35,
            focus_follows_mouse: false,
            mini_bar_side: MiniBarSide::Bottom,
            mini_bar_offset: 0.5,
        }
    }
}

// ---- loading (with the settings written before views could move) -------------------

/// `tab_position` as any version wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
enum RawTabPosition {
    Sidebar,
    Top,
    /// Older: a separate vertical "Sessions" panel on that side.
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct LegacySection {
    kind: View,
}

#[derive(Deserialize)]
#[serde(default)]
struct RawLayout {
    tab_position: Option<RawTabPosition>,
    group_by_pane: bool,
    compact_tabs: bool,
    sidebar_side: Side,
    views: Option<ViewLayout>,
    dim_inactive_panes: bool,
    dim_amount: f32,
    focus_follows_mouse: bool,
    mini_bar_side: MiniBarSide,
    mini_bar_offset: f32,
    // Older: one sidebar with stacked sections and an optional activity bar.
    sections: Option<Vec<LegacySection>>,
    activity_bar: bool,
    active_view: Option<View>,
    sidebar_visible: bool,
    sidebar_width: Option<f32>,
    tabs_width: Option<f32>,
}

impl Default for RawLayout {
    fn default() -> Self {
        let d = LayoutSettings::default();
        Self {
            tab_position: None,
            group_by_pane: d.group_by_pane,
            compact_tabs: d.compact_tabs,
            sidebar_side: d.sidebar_side,
            views: None,
            dim_inactive_panes: d.dim_inactive_panes,
            dim_amount: d.dim_amount,
            focus_follows_mouse: d.focus_follows_mouse,
            mini_bar_side: d.mini_bar_side,
            mini_bar_offset: d.mini_bar_offset,
            sections: None,
            activity_bar: false,
            active_view: None,
            sidebar_visible: true,
            sidebar_width: None,
            tabs_width: None,
        }
    }
}

impl From<RawLayout> for LayoutSettings {
    fn from(r: RawLayout) -> Self {
        let tab_position = match r.tab_position {
            Some(RawTabPosition::Top) => TabPosition::Top,
            Some(_) | None => TabPosition::Sidebar,
        };
        let mut views = match r.views.clone() {
            Some(v) => v,
            None => migrate_views(&r),
        };
        views.normalize();
        Self {
            tab_position,
            group_by_pane: r.group_by_pane,
            compact_tabs: r.compact_tabs,
            sidebar_side: r.sidebar_side,
            views,
            dim_inactive_panes: r.dim_inactive_panes,
            dim_amount: r.dim_amount,
            focus_follows_mouse: r.focus_follows_mouse,
            mini_bar_side: r.mini_bar_side,
            mini_bar_offset: sane(r.mini_bar_offset, 0.05, 0.95, 0.5),
        }
    }
}

/// Views from the old single sidebar: its sections (in their order) become the primary
/// side bar's icons, a hidden sidebar becomes a folded one, and a separate vertical
/// tabs panel on the far side becomes the secondary side bar holding Sessions.
fn migrate_views(r: &RawLayout) -> ViewLayout {
    let mut v = ViewLayout::default();
    if let Some(sections) = &r.sections {
        let mut order: Vec<View> = Vec::new();
        for s in sections {
            if !order.contains(&s.kind) {
                order.push(s.kind);
            }
        }
        v.primary.views = order;
        v.normalize();
        // The activity bar showed one section; stacked sections all showed, and the
        // new default leads with Sessions.
        v.primary.active = Some(if r.activity_bar { r.active_view.unwrap_or(View::Hosts) } else { View::Sessions });
        v.primary.collapsed = !r.sidebar_visible;
    }
    if let Some(w) = r.sidebar_width {
        v.primary.size = w;
    }
    let tabs_side = match r.tab_position {
        Some(RawTabPosition::Left) => Some(Side::Left),
        Some(RawTabPosition::Right) => Some(Side::Right),
        _ => None,
    };
    if let Some(side) = tabs_side
        && side != r.sidebar_side
    {
        let shown = v.primary.active;
        v.move_view(View::Sessions, Container::Secondary, None);
        v.secondary.size = r.tabs_width.unwrap_or(SIDE_DEFAULT);
        // The old sidebar didn't list Sessions next to vertical tabs: keep what it showed.
        if shown != Some(View::Sessions) {
            v.primary.active = shown;
        }
    }
    v.normalize();
    v
}

/// Insertion index for a drag at `x` (or `y`) over items whose centres are `centres`
/// (in order): the first item whose centre lies past the pointer.
pub fn insertion_index(centres: &[f32], pos: f32) -> usize {
    centres.iter().position(|c| pos < *c).unwrap_or(centres.len())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// The pane to move focus to from `from` in `dir`: among the rects lying that way,
/// prefer ones that overlap `from` across the axis, then the nearest edge, then the
/// smallest offset of centres. Returns an index into `others`.
pub fn neighbor(from: egui::Rect, others: &[egui::Rect], dir: Direction) -> Option<usize> {
    const SLACK: f32 = 2.0;
    let c = from.center();
    others
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let (gap, overlap, offset) = match dir {
                Direction::Left if r.center().x < c.x && r.right() <= from.left() + SLACK => {
                    (from.left() - r.right(), r.y_range().intersects(from.y_range()), (r.center().y - c.y).abs())
                }
                Direction::Right if r.center().x > c.x && r.left() >= from.right() - SLACK => {
                    (r.left() - from.right(), r.y_range().intersects(from.y_range()), (r.center().y - c.y).abs())
                }
                Direction::Up if r.center().y < c.y && r.bottom() <= from.top() + SLACK => {
                    (from.top() - r.bottom(), r.x_range().intersects(from.x_range()), (r.center().x - c.x).abs())
                }
                Direction::Down if r.center().y > c.y && r.top() >= from.bottom() - SLACK => {
                    (r.top() - from.bottom(), r.x_range().intersects(from.x_range()), (r.center().x - c.x).abs())
                }
                _ => return None,
            };
            Some((i, !overlap, gap.max(0.0), offset))
        })
        .min_by(|a, b| (a.1, a.2, a.3).partial_cmp(&(b.1, b.2, b.3)).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, ..)| i)
}

fn sane(v: f32, min: f32, max: f32, default: f32) -> f32 {
    if v.is_finite() { v.clamp(min, max) } else { default }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insertion_index_follows_centres() {
        let c = [10.0, 30.0, 50.0];
        assert_eq!(insertion_index(&c, 0.0), 0);
        assert_eq!(insertion_index(&c, 20.0), 1);
        assert_eq!(insertion_index(&c, 49.0), 2);
        assert_eq!(insertion_index(&c, 80.0), 3);
        assert_eq!(insertion_index(&[], 5.0), 0);
    }

    #[test]
    fn dim_is_clamped() {
        let l = LayoutSettings { dim_amount: -1.0, ..Default::default() };
        assert_eq!(l.dim_amount(), 0.0);
        assert_eq!(LayoutSettings { dim_amount: f32::NAN, ..Default::default() }.dim_amount(), 0.35);
    }

    #[test]
    fn new_settings_round_trip() {
        let mut l = LayoutSettings { tab_position: TabPosition::Top, sidebar_side: Side::Right, ..Default::default() };
        l.views.move_view(View::Agents, Container::Panel, None);
        l.views.secondary.bar = BarOrientation::Top;
        let json = serde_json::to_string(&l).unwrap();
        assert!(!json.contains("sections") && !json.contains("activity_bar"), "legacy keys are not written: {json}");
        let back: LayoutSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, l);
    }

    #[test]
    fn old_settings_without_layout_load_with_defaults() {
        let old = r#"{"appearance":{"theme":"Nord"},"terminal":{"font_size":15.0},"keybindings":{"CloseTab":null}}"#;
        let s: crate::settings::Settings = serde_json::from_str(old).unwrap();
        assert_eq!(s.layout, LayoutSettings::default());
        assert_eq!(s.layout.tab_position, TabPosition::Sidebar);
        assert!(s.layout.views.is_shown(View::Sessions));
        assert_eq!(s.appearance.theme, "Nord");
        let partial: crate::settings::Settings = serde_json::from_str(r#"{"layout":{"dim_inactive_panes":true}}"#).unwrap();
        assert!(partial.layout.dim_inactive_panes);
        assert_eq!(partial.layout.views, ViewLayout::default());
    }

    /// A settings file from before views: tabs on top, activity bar off, sections
    /// reordered, sidebar hidden.
    #[test]
    fn old_sidebar_sections_become_the_primary_strip() {
        let old = r#"{"tab_position":"Top","sidebar_side":"Right","sidebar_visible":false,"sidebar_width":300.0,"activity_bar":false,"active_view":"Hosts",
            "sections":[{"kind":"Workspaces","collapsed":false},{"kind":"Hosts","collapsed":true},{"kind":"Sessions","collapsed":false},{"kind":"Agents","collapsed":false}]}"#;
        let l: LayoutSettings = serde_json::from_str(old).unwrap();
        assert_eq!(l.tab_position, TabPosition::Top);
        assert_eq!(l.sidebar_side, Side::Right);
        assert_eq!(l.views.primary.views, vec![View::Workspaces, View::Hosts, View::Sessions, View::Agents, View::Codex]);
        assert_eq!(l.views.primary.active, Some(View::Sessions));
        assert!(l.views.primary.collapsed, "a hidden sidebar is a folded one");
        assert_eq!(l.views.size(Container::Primary), 300.0);
        assert!(l.views.secondary.is_empty() && l.views.panel.is_empty());
    }

    #[test]
    fn old_activity_bar_keeps_its_view() {
        let old = r#"{"activity_bar":true,"active_view":"Agents","sections":[{"kind":"Hosts"},{"kind":"Sessions"},{"kind":"Workspaces"},{"kind":"Agents"}]}"#;
        let l: LayoutSettings = serde_json::from_str(old).unwrap();
        assert_eq!(l.views.primary.active, Some(View::Agents));
        assert!(!l.views.primary.collapsed);
    }

    /// Vertical tabs used to have a panel of their own: on the far side it becomes the
    /// secondary side bar holding Sessions; on the sidebar's side it just joins it.
    #[test]
    fn old_vertical_tabs_panel_maps_to_a_container() {
        let far = r#"{"tab_position":"Right","sidebar_side":"Left","tabs_width":250.0,"activity_bar":true,"active_view":"Hosts",
            "sections":[{"kind":"Hosts"},{"kind":"Sessions"},{"kind":"Workspaces"},{"kind":"Agents"}]}"#;
        let l: LayoutSettings = serde_json::from_str(far).unwrap();
        assert_eq!(l.tab_position, TabPosition::Sidebar);
        assert_eq!(l.views.secondary.views, vec![View::Sessions]);
        assert_eq!(l.views.size(Container::Secondary), 250.0);
        assert_eq!(l.views.primary.active, Some(View::Hosts));
        assert!(l.tab_bars_hidden());
        let near: LayoutSettings = serde_json::from_str(r#"{"tab_position":"Left"}"#).unwrap();
        assert_eq!(near.tab_position, TabPosition::Sidebar);
        assert_eq!(near.views.container_of(View::Sessions), Some(Container::Primary));
        assert!(near.views.secondary.is_empty());
    }

    #[test]
    fn tab_bars_come_back_when_sessions_is_not_on_screen() {
        let mut l = LayoutSettings::default();
        assert!(l.tab_bars_hidden());
        l.views.click(View::Hosts);
        assert!(!l.tab_bars_hidden());
        l.views.move_view(View::Sessions, Container::Panel, None);
        assert!(l.tab_bars_hidden());
        l.views.toggle(Container::Panel);
        assert!(!l.tab_bars_hidden());
        l.tab_position = TabPosition::Top;
        l.views.toggle(Container::Panel);
        assert!(!l.tab_bars_hidden());
    }

    #[test]
    fn containers_dock_on_sides() {
        let mut l = LayoutSettings::default();
        assert_eq!(l.side_of(Container::Primary), Some(Side::Left));
        assert_eq!(l.container_at(Side::Right), Container::Secondary);
        l.sidebar_side = Side::Right;
        assert_eq!(l.side_of(Container::Secondary), Some(Side::Left));
        assert_eq!(l.side_of(Container::Panel), None);
        l.views.move_view(View::Hosts, Container::Panel, None);
        l.reset_views();
        assert_eq!(l.views, ViewLayout::default());
        assert_eq!(l.sidebar_side, Side::Left);
    }

    #[test]
    fn neighbors_follow_the_split_tree_geometry() {
        use egui::{Rect, pos2};
        // Left half | right half split top / bottom.
        let left = Rect::from_min_max(pos2(0.0, 0.0), pos2(500.0, 600.0));
        let top_right = Rect::from_min_max(pos2(506.0, 0.0), pos2(1000.0, 300.0));
        let bottom_right = Rect::from_min_max(pos2(506.0, 306.0), pos2(1000.0, 600.0));
        let all = [left, top_right, bottom_right];
        assert_eq!(neighbor(left, &all, Direction::Right), Some(1), "ties go to the first overlapping pane");
        assert_eq!(neighbor(bottom_right, &all, Direction::Left), Some(0));
        assert_eq!(neighbor(top_right, &all, Direction::Down), Some(2));
        assert_eq!(neighbor(bottom_right, &all, Direction::Up), Some(1));
        assert_eq!(neighbor(left, &all, Direction::Left), None);
        assert_eq!(neighbor(top_right, &all, Direction::Up), None);
    }

    #[test]
    fn positions_cycle() {
        assert_eq!(TabPosition::Top.next().next(), TabPosition::Top);
        assert_eq!(Side::Left.flip(), Side::Right);
    }
}
