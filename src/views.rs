//! The VS Code-style view model: views (Sessions, Hosts, Workspaces, Agents) live in
//! view containers (primary side bar, secondary side bar, bottom panel), each with an
//! ordered icon strip and one active view. Views can be moved between containers,
//! reordered and hidden. Pure data: the UI is in `ui::activity` and `app_views.rs`.

use serde::{Deserialize, Serialize};

/// A view: one icon in an activity strip, one body in its container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum View {
    Hosts,
    /// Every open tab (the vertical tab list).
    Sessions,
    Workspaces,
    /// AI agents running in terminals.
    Agents,
    /// Dedicated Codex terminal sessions.
    Codex,
}

impl View {
    /// Every view, in the default order of the primary side bar.
    pub const ALL: [View; 5] = [View::Sessions, View::Hosts, View::Workspaces, View::Agents, View::Codex];

    /// The Claude section keeps support for other detected CLIs; Codex has its own view.
    pub fn includes_agent(self, kind: crate::agents::AgentKind) -> bool {
        match self {
            View::Agents => kind != crate::agents::AgentKind::Codex,
            View::Codex => kind == crate::agents::AgentKind::Codex,
            _ => false,
        }
    }

    pub fn agent(self) -> Option<crate::agents::AgentKind> {
        match self {
            View::Agents => Some(crate::agents::AgentKind::Claude),
            View::Codex => Some(crate::agents::AgentKind::Codex),
            _ => None,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            View::Hosts => "Hosts",
            View::Sessions => "Sessions",
            View::Workspaces => "Workspaces",
            View::Agents => "Agents",
            View::Codex => "Codex",
        }
    }
}

/// Where views can live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Container {
    /// The side bar on `LayoutSettings::sidebar_side` (left by default).
    Primary,
    /// The side bar on the other side.
    Secondary,
    /// Below the terminals.
    Panel,
}

impl Container {
    pub const ALL: [Container; 3] = [Container::Primary, Container::Secondary, Container::Panel];

    pub fn title(self) -> &'static str {
        match self {
            Container::Primary => "Primary side bar",
            Container::Secondary => "Secondary side bar",
            Container::Panel => "Panel",
        }
    }

    pub fn is_side(self) -> bool {
        self != Container::Panel
    }
}

/// Where a side bar's icon strip sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BarOrientation {
    /// A vertical strip on the side bar's outer edge.
    #[default]
    Side,
    /// A row of icons along the top of the side bar.
    Top,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContainerState {
    /// Views in strip order.
    pub views: Vec<View>,
    /// The view shown (one of `views`; repaired by [`ViewLayout::normalize`]).
    pub active: Option<View>,
    /// Folded away: a side bar keeps only its icon strip, the panel only its tab row.
    pub collapsed: bool,
    /// Width (side bars) or height (panel), in points.
    pub size: f32,
    /// Side bars only.
    pub bar: BarOrientation,
}

impl Default for ContainerState {
    fn default() -> Self {
        // 0 = not set: [`ViewLayout::size`] picks the container's default.
        Self { views: Vec::new(), active: None, collapsed: false, size: 0.0, bar: BarOrientation::Side }
    }
}

impl ContainerState {
    fn with(views: &[View], size: f32) -> Self {
        Self { views: views.to_vec(), active: views.first().copied(), size, ..Default::default() }
    }

    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }

    /// The view whose body is on screen, if any.
    pub fn shown(&self) -> Option<View> {
        if self.collapsed { None } else { self.active.filter(|v| self.views.contains(v)) }
    }
}

pub const SIDE_DEFAULT: f32 = 264.0;
pub const SIDE_MIN: f32 = 200.0;
pub const SIDE_MAX: f32 = 520.0;
pub const PANEL_DEFAULT: f32 = 220.0;
pub const PANEL_MIN: f32 = 96.0;
pub const PANEL_MAX: f32 = 640.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewLayout {
    pub primary: ContainerState,
    pub secondary: ContainerState,
    pub panel: ContainerState,
    /// Views taken off every strip ("Hide"); they come back from a strip's menu.
    pub hidden: Vec<View>,
}

impl Default for ViewLayout {
    fn default() -> Self {
        Self {
            primary: ContainerState::with(&View::ALL, SIDE_DEFAULT),
            secondary: ContainerState::with(&[], SIDE_DEFAULT),
            panel: ContainerState::with(&[], PANEL_DEFAULT),
            hidden: Vec::new(),
        }
    }
}

impl ViewLayout {
    pub fn get(&self, c: Container) -> &ContainerState {
        match c {
            Container::Primary => &self.primary,
            Container::Secondary => &self.secondary,
            Container::Panel => &self.panel,
        }
    }

    pub fn get_mut(&mut self, c: Container) -> &mut ContainerState {
        match c {
            Container::Primary => &mut self.primary,
            Container::Secondary => &mut self.secondary,
            Container::Panel => &mut self.panel,
        }
    }

    /// The container holding `view` (`None` while it is hidden).
    pub fn container_of(&self, view: View) -> Option<Container> {
        Container::ALL.into_iter().find(|c| self.get(*c).views.contains(&view))
    }

    /// Whether `view`'s body is on screen.
    pub fn is_shown(&self, view: View) -> bool {
        self.container_of(view).is_some_and(|c| self.get(c).shown() == Some(view))
    }

    /// Size of `c`, kept inside its limits (a hand-edited file can't break the layout).
    pub fn size(&self, c: Container) -> f32 {
        let (min, max, default) = if c.is_side() { (SIDE_MIN, SIDE_MAX, SIDE_DEFAULT) } else { (PANEL_MIN, PANEL_MAX, PANEL_DEFAULT) };
        let v = self.get(c).size;
        if v.is_finite() && v > 0.0 { v.clamp(min, max) } else { default }
    }

    /// Every view exactly once (the first occurrence wins, missing ones go to the
    /// primary side bar) and every non-empty container with a valid active view.
    pub fn normalize(&mut self) {
        let mut seen: Vec<View> = Vec::with_capacity(View::ALL.len());
        for c in Container::ALL {
            self.get_mut(c).views.retain(|v| {
                let fresh = !seen.contains(v);
                if fresh {
                    seen.push(*v);
                }
                fresh
            });
        }
        self.hidden.retain(|v| {
            let fresh = !seen.contains(v);
            if fresh {
                seen.push(*v);
            }
            fresh
        });
        for v in View::ALL {
            if !seen.contains(&v) {
                self.primary.views.push(v);
            }
        }
        for c in Container::ALL {
            let s = self.get_mut(c);
            if !s.active.is_some_and(|a| s.views.contains(&a)) {
                s.active = s.views.first().copied();
            }
        }
    }

    /// Moves `view` into `to` before the view now at `index` (`None` = the end), makes
    /// it the active view there and opens that container. `index` counts the target
    /// strip as it is before the move (the way a drop indicator shows it).
    pub fn move_view(&mut self, view: View, to: Container, index: Option<usize>) {
        let from = self.container_of(view);
        let mut index = index.unwrap_or(usize::MAX);
        if let Some(c) = from {
            let src = self.get_mut(c);
            let at = src.views.iter().position(|v| *v == view).unwrap_or(0);
            src.views.remove(at);
            if c == to && at < index {
                index -= 1;
            }
            if c != to && src.active == Some(view) {
                // The neighbour that slides into its place takes over.
                src.active = src.views.get(at).or_else(|| src.views.last()).copied();
            }
        }
        self.hidden.retain(|v| *v != view);
        let dst = self.get_mut(to);
        let index = index.min(dst.views.len());
        dst.views.insert(index, view);
        dst.active = Some(view);
        dst.collapsed = false;
    }

    /// Takes `view` off its strip.
    pub fn hide(&mut self, view: View) {
        let Some(c) = self.container_of(view) else { return };
        let s = self.get_mut(c);
        let at = s.views.iter().position(|v| *v == view).unwrap_or(0);
        s.views.remove(at);
        if s.active == Some(view) {
            s.active = s.views.get(at).or_else(|| s.views.last()).copied();
        }
        if !self.hidden.contains(&view) {
            self.hidden.push(view);
        }
    }

    /// Shows `view`: un-hides it (onto the primary side bar), makes it active and opens
    /// its container.
    pub fn reveal(&mut self, view: View) {
        let c = match self.container_of(view) {
            Some(c) => c,
            None => {
                self.move_view(view, Container::Primary, None);
                return;
            }
        };
        let s = self.get_mut(c);
        s.active = Some(view);
        s.collapsed = false;
    }

    /// A click on `view`'s icon: switch to it, or fold the container when it is the
    /// view already showing.
    pub fn click(&mut self, view: View) {
        if self.is_shown(view) {
            if let Some(c) = self.container_of(view) {
                self.get_mut(c).collapsed = true;
            }
        } else {
            self.reveal(view);
        }
    }

    /// Folds or opens `c` (a no-op for an empty container).
    pub fn toggle(&mut self, c: Container) {
        let s = self.get_mut(c);
        if !s.is_empty() {
            s.collapsed = !s.collapsed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(l: &ViewLayout, c: Container) -> Vec<View> {
        l.get(c).views.clone()
    }

    #[test]
    fn defaults_put_every_view_in_the_primary_bar_with_sessions_active() {
        let l = ViewLayout::default();
        assert_eq!(order(&l, Container::Primary), View::ALL.to_vec());
        assert_eq!(l.primary.active, Some(View::Sessions));
        assert!(l.secondary.is_empty() && l.panel.is_empty());
        assert!(l.is_shown(View::Sessions) && !l.is_shown(View::Hosts));
    }

    #[test]
    fn moving_between_containers_hands_over_the_active_view() {
        let mut l = ViewLayout::default();
        l.move_view(View::Sessions, Container::Secondary, None);
        assert_eq!(order(&l, Container::Secondary), vec![View::Sessions]);
        assert_eq!(l.secondary.active, Some(View::Sessions));
        assert_eq!(l.primary.active, Some(View::Hosts), "the neighbour slides in");
        assert_eq!(l.container_of(View::Sessions), Some(Container::Secondary));
        l.move_view(View::Agents, Container::Panel, Some(0));
        assert_eq!(l.container_of(View::Agents), Some(Container::Panel));
        assert!(l.is_shown(View::Agents));
        // The last view of a strip moved away leaves the container empty (it disappears).
        l.move_view(View::Agents, Container::Secondary, Some(0));
        assert!(l.panel.is_empty() && l.panel.active.is_none());
        assert_eq!(order(&l, Container::Secondary), vec![View::Agents, View::Sessions]);
    }

    #[test]
    fn reordering_within_a_strip_counts_indices_before_the_move() {
        let mut l = ViewLayout::default();
        // Sessions, Hosts, Workspaces, Agents → drop Sessions before Agents (index 3).
        l.move_view(View::Sessions, Container::Primary, Some(3));
        assert_eq!(order(&l, Container::Primary), vec![View::Hosts, View::Workspaces, View::Sessions, View::Agents, View::Codex]);
        l.move_view(View::Agents, Container::Primary, Some(0));
        assert_eq!(order(&l, Container::Primary), vec![View::Agents, View::Hosts, View::Workspaces, View::Sessions, View::Codex]);
        l.move_view(View::Hosts, Container::Primary, None);
        assert_eq!(order(&l, Container::Primary), vec![View::Agents, View::Workspaces, View::Sessions, View::Codex, View::Hosts]);
        // Onto itself: no change.
        l.move_view(View::Workspaces, Container::Primary, Some(1));
        l.move_view(View::Workspaces, Container::Primary, Some(2));
        assert_eq!(order(&l, Container::Primary), vec![View::Agents, View::Workspaces, View::Sessions, View::Codex, View::Hosts]);
    }

    #[test]
    fn clicking_the_active_icon_folds_and_another_switches() {
        let mut l = ViewLayout::default();
        l.click(View::Sessions);
        assert!(l.primary.collapsed && !l.is_shown(View::Sessions));
        l.click(View::Sessions);
        assert!(l.is_shown(View::Sessions));
        l.click(View::Hosts);
        assert!(l.is_shown(View::Hosts) && !l.primary.collapsed);
        l.toggle(Container::Primary);
        assert!(l.primary.collapsed);
        l.toggle(Container::Panel);
        assert!(!l.panel.collapsed, "empty containers don't fold");
    }

    #[test]
    fn hidden_views_come_back_on_reveal() {
        let mut l = ViewLayout::default();
        l.hide(View::Sessions);
        assert_eq!(l.container_of(View::Sessions), None);
        assert_eq!(l.hidden, vec![View::Sessions]);
        assert_eq!(l.primary.active, Some(View::Hosts));
        l.reveal(View::Sessions);
        assert_eq!(l.container_of(View::Sessions), Some(Container::Primary));
        assert!(l.hidden.is_empty() && l.is_shown(View::Sessions));
    }

    #[test]
    fn normalize_repairs_duplicates_missing_views_and_stale_active() {
        let mut l: ViewLayout = serde_json::from_str(
            r#"{"primary":{"views":["Hosts","Hosts"],"active":"Agents"},"panel":{"views":["Hosts","Agents"]},"hidden":["Agents","Workspaces"]}"#,
        )
        .unwrap();
        l.normalize();
        assert_eq!(order(&l, Container::Primary), vec![View::Hosts, View::Sessions, View::Codex]);
        assert_eq!(order(&l, Container::Panel), vec![View::Agents]);
        assert_eq!(l.hidden, vec![View::Workspaces]);
        assert_eq!(l.primary.active, Some(View::Hosts));
        assert_eq!(l.panel.active, Some(View::Agents));
        assert_eq!(l.size(Container::Panel), PANEL_DEFAULT, "a missing panel size falls back");
    }

    #[test]
    fn sizes_are_clamped() {
        let mut l = ViewLayout::default();
        l.primary.size = 9999.0;
        l.panel.size = f32::NAN;
        assert_eq!(l.size(Container::Primary), SIDE_MAX);
        assert_eq!(l.size(Container::Panel), PANEL_DEFAULT);
    }

    #[test]
    fn codex_is_added_to_existing_layouts_and_moves_independently() {
        let mut l: ViewLayout = serde_json::from_str(
            r#"{"primary":{"views":["Hosts","Sessions"],"active":"Hosts"},"panel":{"views":["Agents"],"active":"Agents"},"hidden":["Workspaces"]}"#,
        )
        .unwrap();
        l.normalize();
        assert_eq!(l.container_of(View::Codex), Some(Container::Primary));
        assert_eq!(l.primary.active, Some(View::Hosts));
        l.move_view(View::Codex, Container::Secondary, None);
        assert!(l.is_shown(View::Codex) && l.is_shown(View::Agents));
        l.hide(View::Codex);
        assert!(l.is_shown(View::Agents));
        l.reveal(View::Codex);
        assert!(l.is_shown(View::Codex));
        assert!(View::Codex.includes_agent(crate::agents::AgentKind::Codex));
        assert!(!View::Codex.includes_agent(crate::agents::AgentKind::Claude));
        assert!(!View::Agents.includes_agent(crate::agents::AgentKind::Codex));
        assert!(View::Agents.includes_agent(crate::agents::AgentKind::Claude));
    }

    #[test]
    fn serde_round_trips() {
        let mut l = ViewLayout::default();
        l.move_view(View::Agents, Container::Panel, None);
        l.secondary.bar = BarOrientation::Top;
        let back: ViewLayout = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        assert_eq!(back, l);
    }
}
