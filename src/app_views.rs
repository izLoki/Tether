//! The view containers around the terminals: the primary and secondary side bars (each
//! with its activity strip) and the bottom panel, the views inside them, and what
//! happens when icons are clicked, dragged or picked from their menus. A child module
//! of `app`; the model is `crate::views`, the strips `ui::activity`.

use eframe::egui::{self, Align2, FontId, Pos2, Rect, Sense, Ui, UiBuilder, Vec2};

use super::TetherApp;
use crate::layout::{BarOrientation, Container, ContainerState, LayoutSettings, Side, TabPosition, View};
use crate::settings;
use crate::theme;
use crate::ui::activity::{self, Badge, DropTargets, Strip, StripStyle, ViewEvent};
use crate::ui::agents_panel::{self, PanelAction};
use crate::ui::settings_view::SettingsView;
use crate::ui::sidebar::{Sidebar, SidebarAction, SidebarData};
use crate::ui::vertical_tabs::{ListOptions, TabAction, VerticalTabs};
use crate::views::{PANEL_MAX, PANEL_MIN, SIDE_MAX, SIDE_MIN};

const HEADER_H: f32 = 32.0;
/// Horizontal padding inside a side bar.
const PAD: i8 = 8;
/// Bottom margin of the panel.
const PANEL_BOTTOM: i8 = 6;

/// Gives the views their content and collects what they ask for.
struct Host<'a, 'd> {
    sidebar: &'a mut Sidebar,
    data: &'a SidebarData<'d>,
    dock: &'a mut egui_dock::DockState<crate::ui::tabs::Tab>,
    vtabs: &'a mut VerticalTabs,
    opts: ListOptions,
    position: TabPosition,
    agents: &'a [crate::agents::SessionInfo],
    actions: Vec<SidebarAction>,
    tab_actions: Vec<TabAction>,
    agent_action: Option<PanelAction>,
}

impl Host<'_, '_> {
    fn count(&self, v: View) -> Option<usize> {
        match v {
            View::Hosts => Some(Sidebar::host_count(self.data)),
            View::Sessions => Some(self.dock.iter_all_tabs().count() + self.vtabs.extra_rows.len()),
            View::Workspaces => Some(self.data.workspaces.len()),
            View::Agents | View::Codex => Some(self.agents.iter().filter(|s| v.includes_agent(s.kind)).count()),
        }
    }

    /// The view's buttons, in a right-to-left `ui`.
    fn header_buttons(&mut self, v: View, ui: &mut Ui) {
        match v {
            View::Hosts => {
                let a = self.sidebar.hosts_header(ui, self.data);
                self.actions.extend(a);
            }
            View::Sessions => {
                let a = VerticalTabs::header_buttons(ui, self.opts, self.position);
                self.tab_actions.extend(a);
            }
            View::Workspaces => self.sidebar.workspaces_header(ui, self.data),
            View::Agents | View::Codex => {
                if crate::ui::settings_widgets::icon_button(ui, crate::ui::settings_widgets::Icon::Plus, 24.0).on_hover_text("Launch agent").clicked() {
                    let kind = v.agent().unwrap();
                    self.agent_action = Some(PanelAction::Launch(agents_panel::LaunchPick {
                        kind,
                        command: agents_panel::default_command(kind).into(),
                        shell_id: None,
                        cwd: None,
                    }));
                }
                if crate::ui::settings_widgets::icon_button(ui, crate::ui::settings_widgets::Icon::Gear, 24.0)
                    .on_hover_text("Agent settings and hooks")
                    .clicked()
                {
                    self.agent_action = Some(PanelAction::InstallHooks);
                }
            }
        }
    }

    fn body(&mut self, v: View, ui: &mut Ui, in_panel: bool) {
        match v {
            View::Hosts => {
                let a = self.sidebar.hosts_body(ui, self.data);
                self.actions.extend(a);
            }
            View::Sessions => {
                let a = if in_panel { self.vtabs.columns(ui, self.dock, self.opts) } else { self.vtabs.body(ui, self.dock, self.opts, self.position) };
                self.tab_actions.extend(a);
            }
            View::Workspaces => {
                egui::ScrollArea::vertical().id_salt(("view_workspaces", in_panel)).auto_shrink([false, false]).show(ui, |ui| {
                    let a = self.sidebar.workspaces_body(ui, self.data);
                    self.actions.extend(a);
                });
            }
            View::Agents | View::Codex => {
                egui::ScrollArea::vertical().id_salt(("view_agents", v, in_panel)).auto_shrink([false, false]).show(ui, |ui| {
                    if let Some(action) = agents_panel::list_for(ui, self.agents, v, std::time::Instant::now()) {
                        self.agent_action = Some(action);
                    }
                });
                // Elapsed times tick while sessions are listed.
                if !self.agents.is_empty() {
                    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
                }
            }
        }
    }

    /// Title (with a faint count) on the left, the view's buttons on the right.
    fn header(&mut self, v: View, ui: &mut Ui) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), HEADER_H), Sense::hover());
        let p = ui.painter();
        let galley = p.layout_no_wrap(v.title().to_uppercase(), FontId::proportional(10.5), theme::text_faint());
        let w = galley.size().x;
        p.galley(Pos2::new(rect.left() + 4.0, rect.center().y - galley.size().y / 2.0), galley, theme::text_faint());
        if let Some(n) = self.count(v).filter(|n| *n > 0) {
            p.text(
                Pos2::new(rect.left() + 4.0 + w + 6.0, rect.center().y),
                Align2::LEFT_CENTER,
                n.to_string(),
                FontId::proportional(10.5),
                theme::text_faint().gamma_multiply(0.7),
            );
        }
        let mut buttons = ui.new_child(UiBuilder::new().max_rect(rect).layout(egui::Layout::right_to_left(egui::Align::Center)));
        self.header_buttons(v, &mut buttons);
    }
}

fn side_panel(id: egui::Id, side: Side) -> egui::Panel {
    match side {
        Side::Left => egui::Panel::left(id),
        Side::Right => egui::Panel::right(id),
    }
}

/// Everything a container needs besides the views' content.
struct Frame<'a> {
    l: &'a LayoutSettings,
    badge: &'a dyn Fn(View) -> Option<Badge>,
    targets: &'a mut DropTargets,
    events: Vec<ViewEvent>,
}

impl TetherApp {
    /// Side bars and the panel, outside in; then the terminals take what is left.
    pub(super) fn view_containers(&mut self, ui: &mut Ui) {
        let l = settings::snapshot().layout.clone();
        let workspaces = self.workspace_infos();
        let pool = self.pool.clone();
        let is_live = move |id| pool.has_live(id);
        let has_tabs = self.layout_specs().is_some();
        let shells = crate::pty::shells::detected().unwrap_or_default();
        let data = SidebarData { store: &self.store, folders: &self.folders, workspaces: &workspaces, has_tabs, is_live: &is_live, shells };
        self.vtabs.extra_rows.clear();
        for tab in self.detached.tabs_mut() {
            self.vtabs.extra_rows.push(crate::ui::vertical_tabs::ExtraSessionRow {
                uid: tab.uid,
                title: tab.display_title(),
                subtitle: tab.subtitle(),
                icon: crate::ui::vertical_tabs::row_icon_for(tab),
                hue: tab.profile.hue,
                status: tab.status_color(),
                minimized: false,
            });
        }
        for tab in self.mini_dock.tabs_mut() {
            self.vtabs.extra_rows.push(crate::ui::vertical_tabs::ExtraSessionRow {
                uid: tab.uid,
                title: tab.display_title(),
                subtitle: tab.subtitle(),
                icon: crate::ui::vertical_tabs::row_icon_for(tab),
                hue: tab.profile.hue,
                status: tab.status_color(),
                minimized: true,
            });
        }
        let tabs = self.dock.iter_all_tabs().count() + self.vtabs.extra_rows.len();
        let agent_badge = |view: View| {
            let sessions = self.agents.sessions().iter().filter(|s| view.includes_agent(s.kind));
            let count = sessions.clone().filter(|s| s.state.needs_attention()).count();
            let urgent = sessions.map(|s| s.state).filter(|s| s.needs_attention()).min_by_key(|s| s.urgency());
            Badge { count, tone: Some(urgent.map_or(theme::warn(), |s| crate::ui::agent_status::tone_color(crate::ui::agent_status::spec(s).tone))) }
        };
        let badges = [agent_badge(View::Agents), agent_badge(View::Codex)];
        let badge = move |v: View| match v {
            View::Sessions => Some(Badge { count: tabs, tone: None }),
            View::Agents => Some(badges[0]),
            View::Codex => Some(badges[1]),
            View::Hosts | View::Workspaces => None,
        };
        let mut host = Host {
            sidebar: &mut self.sidebar,
            data: &data,
            dock: &mut self.dock,
            vtabs: &mut self.vtabs,
            opts: ListOptions { grouped: l.group_by_pane, compact: l.compact_tabs },
            position: l.tab_position,
            agents: self.agents.sessions(),
            actions: Vec::new(),
            tab_actions: Vec::new(),
            agent_action: None,
        };
        self.view_targets.clear();
        let mut f = Frame { l: &l, badge: &badge, targets: &mut self.view_targets, events: Vec::new() };
        side_container(ui, Container::Primary, &mut f, &mut host);
        side_container(ui, Container::Secondary, &mut f, &mut host);
        panel_container(ui, &mut f, &mut host);
        f.targets.central = Some(ui.available_rect_before_wrap());
        let events = std::mem::take(&mut f.events);
        let (actions, tab_actions, agent_action) = (std::mem::take(&mut host.actions), std::mem::take(&mut host.tab_actions), host.agent_action.take());
        drop(host);
        for a in actions {
            self.apply_sidebar(a, ui.ctx());
        }
        self.apply_tab_actions(tab_actions, ui.ctx());
        self.apply_agents_panel(agent_action, ui.ctx());
        self.apply_view_events(events, ui.ctx());
    }

    /// Drop zones and the dragged icon (over everything), the move on release.
    pub(super) fn finish_view_drag(&mut self, ctx: &egui::Context) {
        let l = settings::snapshot().layout.clone();
        if let Some(e) = self.view_targets.finish(ctx, &l) {
            self.apply_view_events(vec![e], ctx);
        }
    }

    pub(super) fn apply_view_events(&mut self, events: Vec<ViewEvent>, ctx: &egui::Context) {
        for e in events {
            match e {
                ViewEvent::OpenSettings => {
                    self.settings_view.get_or_insert_with(SettingsView::new);
                }
                ViewEvent::Reset => reset_layout(ctx),
                ViewEvent::LaunchAgent(kind) => self.launch_agent_command(kind, crate::ui::agents_panel::default_command(kind).into(), ctx),
                ViewEvent::CustomBoot(kind) => self.custom_boot = Some(crate::ui::agents_panel::CustomBoot::new(kind)),
                e => {
                    let _ = settings::update(|s| apply_to(&mut s.layout, e));
                }
            }
        }
    }
}

fn apply_to(l: &mut LayoutSettings, e: ViewEvent) {
    let v = &mut l.views;
    match e {
        ViewEvent::Click(view) => v.click(view),
        ViewEvent::Move { view, to, index } => v.move_view(view, to, index),
        ViewEvent::Hide(view) => v.hide(view),
        ViewEvent::Reveal(view) => v.reveal(view),
        ViewEvent::Toggle(c) => v.toggle(c),
        ViewEvent::SetBar(c, bar) => v.get_mut(c).bar = bar,
        ViewEvent::Reset | ViewEvent::OpenSettings | ViewEvent::LaunchAgent(_) | ViewEvent::CustomBoot(_) => {}
    }
}

/// Default views, sides and sizes.
pub(super) fn reset_layout(ctx: &egui::Context) {
    let _ = settings::update(|s| s.layout.reset_views());
    activity::forget_sizes(ctx);
}

fn side_container(ui: &mut Ui, c: Container, f: &mut Frame<'_>, host: &mut Host<'_, '_>) {
    let state: &ContainerState = f.l.views.get(c);
    if state.is_empty() {
        return;
    }
    let side = f.l.side_of(c).unwrap_or(Side::Left);
    let right = side == Side::Right;
    // A folded side bar with icons on top still needs somewhere to click.
    if state.bar == BarOrientation::Side || state.collapsed {
        let resp = side_panel(egui::Id::new(("view_strip", c)), side)
            .exact_size(activity::STRIP_W)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(theme::bg()))
            .show(ui, |ui| {
                let r = ui.max_rect();
                let strip = Strip { container: c, state, hidden: &f.l.views.hidden, style: StripStyle::Vertical, marker_right: right, badge: f.badge };
                strip.show(ui, r, f.targets, &mut f.events);
                if c == Container::Primary {
                    activity::settings_button(ui, r, &mut f.events);
                }
            });
        let r = resp.response.rect;
        let x = if right { r.left() + 0.5 } else { r.right() - 0.5 };
        ui.painter().vline(x, r.y_range(), egui::Stroke::new(1.0, theme::border().gamma_multiply(0.45)));
    }
    let margin = egui::Margin { left: PAD, right: PAD, top: 0, bottom: 8 };
    let mut open = !state.collapsed;
    let shown = side_panel(activity::container_id(c), side)
        .resizable(true)
        .drag_to_open(false)
        .default_size(f.l.views.size(c))
        .size_range(SIDE_MIN..=SIDE_MAX)
        .frame(egui::Frame::new().fill(theme::bg()).inner_margin(margin))
        .show_separator_line(false)
        .show_collapsible(ui, &mut open, |ui| {
            if state.bar == BarOrientation::Top {
                let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), activity::ROW_H), Sense::hover());
                let strip = Strip { container: c, state, hidden: &f.l.views.hidden, style: StripStyle::Row, marker_right: false, badge: f.badge };
                strip.show(ui, r.expand2(Vec2::new(f32::from(PAD) - 4.0, 0.0)), f.targets, &mut f.events);
                ui.painter().hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, theme::border().gamma_multiply(0.35)));
            } else {
                ui.add_space(4.0);
            }
            if let Some(v) = state.active {
                ui.spacing_mut().item_spacing.y = 0.0;
                host.header(v, ui);
                let rect = Rect::from_min_max(ui.cursor().min, ui.max_rect().max);
                let mut child = ui.new_child(UiBuilder::new().max_rect(rect).id_salt(("view_body", c, v)));
                child.set_clip_rect(rect.intersect(ui.clip_rect()));
                host.body(v, &mut child, false);
                ui.advance_cursor_after_rect(rect);
            }
        });
    if open == state.collapsed {
        // Dragged shut (or open) by its edge.
        f.events.push(ViewEvent::Toggle(c));
    }
    if let Some(r) = shown {
        let edge = r.response.rect;
        let x = if right { edge.left() + 0.5 } else { edge.right() - 0.5 };
        ui.painter().vline(x, edge.y_range(), egui::Stroke::new(1.0, theme::border().gamma_multiply(0.45)));
        f.targets.add_body(c, edge);
        persist_size(ui.ctx(), c, f.l.views.size(c));
    }
}

fn panel_container(ui: &mut Ui, f: &mut Frame<'_>, host: &mut Host<'_, '_>) {
    let state = &f.l.views.panel;
    if state.is_empty() {
        return;
    }
    let frame = egui::Frame::new().fill(theme::bg()).inner_margin(egui::Margin { left: PAD, right: PAD, top: 0, bottom: PANEL_BOTTOM });
    let collapsed = egui::Panel::bottom("view_panel_folded").exact_size(activity::TABS_H + 6.0).resizable(false).show_separator_line(false).frame(frame);
    let expanded = egui::Panel::bottom(activity::container_id(Container::Panel))
        .resizable(true)
        .default_size(f.l.views.size(Container::Panel))
        .size_range(PANEL_MIN..=PANEL_MAX)
        .show_separator_line(false)
        .frame(frame);
    let mut open = !state.collapsed;
    let r = egui::Panel::show_switched(ui, &mut open, collapsed, expanded, |ui, expanded| {
        let (tabs, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), activity::TABS_H), Sense::hover());
        let full = ui.max_rect();
        ui.painter().hline(
            full.expand2(Vec2::new(f32::from(PAD), 0.0)).x_range(),
            full.top() + 0.5,
            egui::Stroke::new(1.0, theme::border().gamma_multiply(0.45)),
        );
        let strip = Strip { container: Container::Panel, state, hidden: &f.l.views.hidden, style: StripStyle::Tabs, marker_right: false, badge: f.badge };
        let strip_w = strip_width(ui, state, f.badge).min(tabs.width() * 0.6);
        strip.show(ui, Rect::from_min_size(tabs.min, Vec2::new(strip_w, tabs.height())), f.targets, &mut f.events);
        // Right side: the view's own buttons, then fold / unfold.
        let mut right = ui.new_child(
            UiBuilder::new()
                .max_rect(Rect::from_min_max(Pos2::new(tabs.left() + strip_w, tabs.top()), tabs.max))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        right.spacing_mut().item_spacing.x = 2.0;
        let tip = crate::keybindings::hint(if expanded { "Fold panel" } else { "Unfold panel" }, crate::keybindings::Action::TogglePanel);
        if fold_button(&mut right, expanded).on_hover_text(tip).clicked() {
            f.events.push(ViewEvent::Toggle(Container::Panel));
        }
        if expanded && let Some(v) = state.active {
            right.add_space(6.0);
            host.header_buttons(v, &mut right);
            let rect = Rect::from_min_max(Pos2::new(full.left(), tabs.bottom() + 4.0), full.max);
            let mut child = ui.new_child(UiBuilder::new().max_rect(rect).id_salt(("view_body", Container::Panel, v)));
            child.set_clip_rect(rect.intersect(ui.clip_rect()));
            host.body(v, &mut child, true);
            ui.advance_cursor_after_rect(rect);
        }
        expanded
    });
    if open == state.collapsed {
        f.events.push(ViewEvent::Toggle(Container::Panel));
    }
    if r.inner {
        let rect = r.response.rect;
        f.targets.add_body(Container::Panel, rect);
        persist_size(ui.ctx(), Container::Panel, f.l.views.size(Container::Panel));
    }
}

fn strip_width(ui: &Ui, state: &ContainerState, badge: &dyn Fn(View) -> Option<Badge>) -> f32 {
    // Measure by laying the tabs out off screen would duplicate ids; the strip is a
    // little wider than its tabs, which is fine for a drop target.
    let per = |v: View| {
        let text = ui.painter().layout_no_wrap(v.title().to_uppercase(), FontId::proportional(11.0), egui::Color32::WHITE).size().x;
        let b = badge(v).filter(|b| b.count > 0).map_or(0.0, |_| 26.0);
        12.0 + 14.0 + 7.0 + text + b + 12.0 + 4.0
    };
    8.0 + state.views.iter().map(|v| per(*v)).sum::<f32>() + 16.0
}

fn fold_button(ui: &mut Ui, expanded: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, if expanded { "Fold panel" } else { "Unfold panel" }));
    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), theme::anim_time(0.1));
    if h > 0.0 {
        ui.painter().rect_filled(rect, egui::CornerRadius::same(6), theme::surface_3().gamma_multiply(h));
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let col = theme::text_dim().lerp_to_gamma(theme::text(), h);
    let c = rect.center();
    let d = if expanded { 1.0 } else { -1.0 };
    let pts = vec![c + Vec2::new(-4.5, -2.0 * d), c + Vec2::new(0.0, 2.5 * d), c + Vec2::new(4.5, -2.0 * d)];
    ui.painter().add(egui::Shape::line(pts, egui::Stroke::new(1.5, col)));
    resp
}

/// Saves a container's size once the user lets go of its edge. Read from egui's own
/// panel state: mid-slide, the panel's response rect is only the part on screen.
fn persist_size(ctx: &egui::Context, c: Container, saved: f32) {
    let Some(state) = egui::PanelState::load(ctx, activity::container_id(c)) else { return };
    // Outer size; the saved one excludes the frame's margins.
    let size = if c.is_side() { state.size().x - 2.0 * f32::from(PAD) } else { state.size().y - f32::from(PANEL_BOTTOM) };
    let (min, max) = if c.is_side() { (SIDE_MIN, SIDE_MAX) } else { (PANEL_MIN, PANEL_MAX) };
    let size = size.clamp(min, max);
    if (size - saved).abs() > 1.0 && !ctx.input(|i| i.pointer.any_down()) {
        let _ = settings::update(|s| s.layout.views.get_mut(c).size = size.round());
    }
}
