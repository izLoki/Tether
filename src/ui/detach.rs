//! Tabs dragged outside the app window become native OS windows (egui immediate
//! viewports), each hosting a small dock of its own so tabs, splits and drags keep
//! working there. Moving such a window back over the app docks its tabs again.
//!
//! Tabs are moved, never rebuilt: SSH sessions survive detach and re-attach.

use std::time::Duration;

use eframe::egui::{self, CornerRadius, FontId, Id, LayerId, Order, Pos2, Rect, Stroke, StrokeKind, Vec2, ViewportBuilder, ViewportCommand, ViewportId, vec2};
use egui_dock::{DockArea, DockState, Node, NodeIndex, NodePath, Split, Surface, SurfaceIndex, TabPath, TabViewer, Tree};

use super::detach_geom::{self as geom, DockGate, DockSide, GateState};
use super::tabs::{Tab, Viewer};
use crate::theme;

/// `DockArea`'s default id, used by the main window's dock.
const MAIN_DOCK_ID: &str = "egui_dock::DockArea";
const DEFAULT_SIZE: Vec2 = vec2(760.0, 460.0);
const ZONE_FADE_SECS: f32 = 0.18;
/// Share of the old content kept when a window docks as a side split.
const SIDE_SPLIT_FRACTION: f32 = 0.6;

/// Follows one tab drag inside one viewport.
#[derive(Default)]
struct DragTrack {
    uid: Option<u64>,
    pos: Option<Pos2>,
    size: Option<Vec2>,
}

/// A tab released outside the viewport it was dragged in, in screen points.
struct OutDrop {
    uid: u64,
    screen: Pos2,
    size: Vec2,
}

struct DetachedWindow {
    key: u64,
    viewport: ViewportId,
    dock_id: Id,
    dock: DockState<Tab>,
    /// Initial outer rect; kept constant so egui never re-sends position/size commands.
    spawn: Rect,
    gate: DockGate,
    track: DragTrack,
    outer: Option<Rect>,
}

struct WindowOut {
    close_requested: bool,
    minimize_requested: bool,
    drop: Option<OutDrop>,
}

/// Owns every detached window and the tabs living in them.
#[derive(Default)]
pub struct Detacher {
    windows: Vec<DetachedWindow>,
    main_track: DragTrack,
    next_key: u64,
    zone: Option<DockSide>,
    last_zone: Option<DockSide>,
}

impl Detacher {
    /// Every tab living in a detached window.
    pub fn tabs(&self) -> impl Iterator<Item = &Tab> {
        self.windows.iter().flat_map(|w| w.dock.iter_all_tabs().map(|(_, t)| t))
    }

    pub fn tabs_mut(&mut self) -> impl Iterator<Item = &mut Tab> {
        self.windows.iter_mut().flat_map(|w| w.dock.iter_all_tabs_mut().map(|(_, t)| t))
    }

    pub fn docks_mut(&mut self) -> impl Iterator<Item = &mut DockState<Tab>> {
        self.windows.iter_mut().map(|w| &mut w.dock)
    }

    /// Removes and returns a tab with `uid` from any detached window.
    pub fn remove_tab_by_uid(&mut self, uid: u64) -> Option<Tab> {
        for w in &mut self.windows {
            if let Some(path) = w.dock.find_tab_from(|t| t.uid == uid) {
                let tab = w.dock.remove_tab(path);
                self.windows.retain(|win| win.dock.iter_all_tabs().next().is_some());
                return tab;
            }
        }
        None
    }

    /// Activates and focuses the detached window holding `uid`.
    pub fn focus_tab(&mut self, ctx: &egui::Context, uid: u64) -> bool {
        for w in &mut self.windows {
            if let Some(path) = w.dock.find_tab_from(|t| t.uid == uid) {
                let _ = w.dock.set_active_tab(path);
                w.dock.set_focused_node_and_surface(path.node_path());
                ctx.send_viewport_cmd_to(w.viewport, ViewportCommand::Focus);
                return true;
            }
        }
        false
    }

    /// Runs once per frame of the main window, after the main dock was shown.
    /// Renders the detached windows (their tab output is collected into `viewer`),
    /// moves tabs between windows, and docks windows that rest over the app.
    pub fn frame(&mut self, ctx: &egui::Context, main: &mut DockState<Tab>, viewer: &mut Viewer, mini_dock: &mut super::mini_dock::MiniDock) {
        let (main_inner, main_outer, content, now) = ctx.input(|i| (i.viewport().inner_rect, i.viewport().outer_rect, i.content_rect(), i.time));
        let mut drops: Vec<(Option<u64>, OutDrop)> = Vec::new();
        if let Some(d) = track_drag(ctx, main, Id::new(MAIN_DOCK_ID), &mut self.main_track) {
            drops.push((None, d));
        }
        if let Some(inner) = main_inner {
            for (tabs, outer) in take_outside_surfaces(ctx, main, content, inner) {
                self.spawn(tabs, outer);
            }
        }
        let mut closed = Vec::new();
        let mut minimized = Vec::new();
        for w in &mut self.windows {
            let out = show_window(ctx, w, viewer);
            if out.close_requested {
                closed.push(w.key);
            } else if out.minimize_requested {
                minimized.push(w.key);
            }
            if let Some(d) = out.drop {
                drops.push((Some(w.key), d));
            }
        }
        for key in minimized {
            if let Some(w) = self.remove(key) {
                let from_rect = super::mini_dock::default_floating_rect(content);
                mini_dock.minimize_tabs(ctx, dock_tabs(w.dock), from_rect);
            }
        }
        for key in closed {
            if let Some(w) = self.remove(key) {
                for mut tab in dock_tabs(w.dock) {
                    // Same as the dock's ✕: terminal sessions are closed.
                    let _ = viewer.on_close(&mut tab);
                }
            }
        }
        for (src, d) in drops {
            self.apply_drop(main, src, d, main_inner);
        }
        self.windows.retain(|w| w.dock.iter_all_tabs().next().is_some());
        self.zone = None;
        if let Some(main_outer) = main_outer {
            self.update_gates(ctx, main, main_outer, now);
        }
        self.paint_zone(ctx);
    }

    /// Moves `tab` into a new native window near `screen_pos` (in screen points).
    pub fn pop_out(&mut self, tab: Tab, screen_pos: Pos2) {
        self.spawn(vec![tab], geom::spawn_rect(screen_pos, DEFAULT_SIZE));
    }

    fn spawn(&mut self, tabs: Vec<Tab>, outer: Rect) {
        if tabs.is_empty() {
            return;
        }
        self.next_key += 1;
        let key = self.next_key;
        self.windows.push(DetachedWindow {
            key,
            viewport: ViewportId::from_hash_of(("tether_detached", key)),
            dock_id: Id::new(("tether_detached_dock", key)),
            dock: DockState::new(tabs),
            spawn: outer,
            gate: DockGate::default(),
            track: DragTrack::default(),
            outer: None,
        });
    }

    fn remove(&mut self, key: u64) -> Option<DetachedWindow> {
        let i = self.windows.iter().position(|w| w.key == key)?;
        Some(self.windows.remove(i))
    }

    fn apply_drop(&mut self, main: &mut DockState<Tab>, src: Option<u64>, d: OutDrop, main_inner: Option<Rect>) {
        let over_window = self.windows.iter().find(|w| Some(w.key) != src && w.outer.is_some_and(|r| r.contains(d.screen))).map(|w| w.key);
        let over_main = over_window.is_none() && main_inner.is_some_and(|r| r.contains(d.screen));
        if over_main && src.is_none() {
            return;
        }
        let lone_in_own_window = src.is_some_and(|k| self.windows.iter().any(|w| w.key == k && w.dock.iter_all_tabs().count() <= 1));
        if !over_main && over_window.is_none() && lone_in_own_window {
            return; // the window itself is what should move; nothing to split off
        }
        let Some(tab) = self.take_tab(main, src, d.uid) else { return };
        if over_main {
            main.push_to_focused_leaf(tab);
        } else if let Some(w) = over_window.and_then(|k| self.windows.iter_mut().find(|w| w.key == k)) {
            w.dock.push_to_focused_leaf(tab);
        } else {
            self.spawn(vec![tab], geom::spawn_rect(d.screen, d.size));
        }
    }

    fn take_tab(&mut self, main: &mut DockState<Tab>, src: Option<u64>, uid: u64) -> Option<Tab> {
        let dock = match src {
            None => main,
            Some(k) => &mut self.windows.iter_mut().find(|w| w.key == k)?.dock,
        };
        let path = dock.find_tab_from(|t| t.uid == uid)?;
        dock.remove_tab(path)
    }

    fn update_gates(&mut self, ctx: &egui::Context, main: &mut DockState<Tab>, main_outer: Rect, now: f64) {
        let mut docking = None;
        for w in &mut self.windows {
            let Some(outer) = w.outer else { continue };
            let side = geom::dock_side(geom::title_probe(outer), main_outer);
            match w.gate.update(outer, main_outer, now) {
                GateState::Idle => {}
                GateState::Hover => {
                    self.zone = Some(side);
                    ctx.request_repaint_after(Duration::from_secs_f64(geom::DOCK_STILL_SECS / 3.0));
                }
                GateState::Dock => {
                    docking = Some((w.key, side));
                    break;
                }
            }
        }
        let Some((key, side)) = docking else { return };
        let Some(w) = self.remove(key) else { return };
        redock(main, dock_tabs(w.dock), side);
        self.zone = None;
        ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Focus);
    }

    /// Accent glow + "Release to dock" pill over the part of the app a window would dock into.
    fn paint_zone(&mut self, ctx: &egui::Context) {
        if self.zone.is_some() {
            self.last_zone = self.zone;
        }
        let t = ctx.animate_bool_with_time(Id::new("tether_dock_zone"), self.zone.is_some(), ZONE_FADE_SECS);
        let Some(side) = self.last_zone.filter(|_| t > 0.0) else { return };
        let rect = geom::zone_rect(side, ctx.content_rect().shrink(6.0));
        let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("tether_dock_zone")));
        let accent = theme::accent();
        let radius = CornerRadius::same(theme::RADIUS);
        let pulse = ((ctx.input(|i| i.time) * 4.0).sin() * 0.5 + 0.5) as f32;
        painter.rect_filled(rect, radius, accent.gamma_multiply((0.07 + 0.04 * pulse) * t));
        for (grow, alpha) in [(0.0, 0.9), (3.0, 0.3), (7.0, 0.12)] {
            painter.rect_stroke(rect.expand(grow), radius, Stroke::new(2.0, accent.gamma_multiply(alpha * t)), StrokeKind::Inside);
        }
        let galley = painter.layout_no_wrap("⬇  Release to dock".into(), FontId::proportional(13.0), theme::bg());
        let pill = Rect::from_center_size(rect.center() + vec2(0.0, (1.0 - t) * 10.0), galley.size() + vec2(28.0, 14.0));
        painter.rect_filled(pill, CornerRadius::same((pill.height() / 2.0) as u8), accent.gamma_multiply(t));
        painter.galley(pill.center() - galley.size() / 2.0, galley, theme::bg().gamma_multiply(t));
        if self.zone.is_some() {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }
}

fn redock(main: &mut DockState<Tab>, tabs: Vec<Tab>, side: DockSide) {
    if tabs.is_empty() {
        return;
    }
    let split = match side {
        DockSide::Left => Some(Split::Left),
        DockSide::Right => Some(Split::Right),
        DockSide::Center => None,
    };
    match split {
        Some(split) if !main.main_surface().is_empty() => {
            let root = NodePath { surface: SurfaceIndex::main(), node: NodeIndex::root() };
            main.split(root, split, SIDE_SPLIT_FRACTION, Node::leaf_with(tabs));
        }
        _ => {
            for tab in tabs {
                main.push_to_focused_leaf(tab);
            }
        }
    }
}

fn show_window(ctx: &egui::Context, w: &mut DetachedWindow, viewer: &mut Viewer) -> WindowOut {
    let builder = ViewportBuilder::default()
        .with_title(window_title(&mut w.dock, viewer))
        .with_position(w.spawn.min)
        .with_inner_size(w.spawn.size())
        .with_min_inner_size(geom::MIN_WINDOW * 0.75);
    ctx.show_viewport_immediate(w.viewport, builder, |ui, _class| {
        let ctx = ui.ctx().clone();
        let (outer, close_requested) = ctx.input(|i| (i.viewport().outer_rect, i.viewport().close_requested()));
        w.outer = outer;
        let mut minimize_requested = false;
        let frame = egui::Frame::new().fill(theme::bg()).inner_margin(egui::Margin::same(0));
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            let full_rect = ui.max_rect();
            let style = theme::dock_style(ui.style());
            DockArea::new(&mut w.dock)
                .id(w.dock_id)
                .style(style)
                .show_add_buttons(false)
                .show_leaf_collapse_buttons(false)
                .show_leaf_close_all_buttons(false)
                .show_inside(ui, viewer);

            // Minimize to mini-dock bar button in the top-right header of the detached window.
            let btn_rect = Rect::from_min_size(Pos2::new(full_rect.right() - 32.0, full_rect.top() + 5.0), Vec2::new(24.0, 20.0));
            let min_resp = ui.interact(btn_rect, w.dock_id.with("min_to_bar"), egui::Sense::click()).on_hover_text("Minimize to mini-bar");
            let bg = if min_resp.hovered() { theme::accent_soft() } else { theme::surface_2().gamma_multiply(0.85) };
            ui.painter().rect_filled(btn_rect, CornerRadius::same(5), bg);
            ui.painter().rect_stroke(btn_rect, CornerRadius::same(5), Stroke::new(1.0, theme::border().gamma_multiply(0.5)), StrokeKind::Inside);
            let col = if min_resp.hovered() { theme::accent() } else { theme::text_dim() };
            let c = btn_rect.center();
            ui.painter().line_segment([c + vec2(-5.0, 1.0), c + vec2(5.0, 1.0)], Stroke::new(1.6, col));
            if min_resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if min_resp.clicked() {
                minimize_requested = true;
            }
        });
        let drop = track_drag(&ctx, &w.dock, w.dock_id, &mut w.track);
        WindowOut { close_requested, minimize_requested, drop }
    })
}

/// "<tab name> — tether", from the window's active tab.
fn window_title(dock: &mut DockState<Tab>, viewer: &mut Viewer) -> String {
    let tab = if dock.find_active_focused().is_some() { dock.find_active_focused().map(|(_, t)| t) } else { dock.iter_all_tabs_mut().next().map(|(_, t)| t) };
    let Some(tab) = tab else { return "tether".into() };
    let fallback = tab.profile.display_name().to_string();
    let text = viewer.title(tab).text().to_string();
    // The dock title is "• <icon>  <name>".
    let name = text.split_once("  ").map(|(_, n)| n.trim().to_string()).filter(|n| !n.is_empty()).unwrap_or(fallback);
    format!("{name} — tether")
}

fn tab_drag_id(dock_id: Id, path: TabPath) -> Id {
    dock_id.with((path.surface, "surface")).with((path.node, "node")).with((path.tab.0, "tab"))
}

/// The tab of `dock` egui is dragging right now, with the size of its leaf.
fn dragged_tab(ctx: &egui::Context, dock: &DockState<Tab>, dock_id: Id) -> Option<(u64, Option<Vec2>)> {
    let dragged = ctx.dragged_id()?;
    let (path, tab) = dock.iter_all_tabs().find(|(path, _)| {
        let id = tab_drag_id(dock_id, *path);
        dragged == id || dragged == id.with("dragged")
    })?;
    Some((tab.uid, dock.leaf(path.node_path()).ok().map(|l| l.rect.size()).filter(|s| s.x > 0.0 && s.y > 0.0)))
}

/// Follows a tab drag in the current viewport; returns it when released outside the viewport.
fn track_drag(ctx: &egui::Context, dock: &DockState<Tab>, dock_id: Id, track: &mut DragTrack) -> Option<OutDrop> {
    let (down, released, pos, decided, content, inner) = ctx.input(|i| {
        (
            i.pointer.primary_down(),
            i.pointer.primary_released(),
            i.pointer.latest_pos(),
            i.pointer.is_decidedly_dragging(),
            i.content_rect(),
            i.viewport().inner_rect,
        )
    });
    if released {
        let t = std::mem::take(track);
        let (uid, pos, inner) = (t.uid?, pos.or(t.pos)?, inner?);
        if content.contains(pos) {
            return None;
        }
        return Some(OutDrop { uid, screen: inner.min + pos.to_vec2(), size: t.size.unwrap_or(DEFAULT_SIZE) });
    }
    if !down {
        *track = DragTrack::default();
        return None;
    }
    if decided && let Some((uid, size)) = dragged_tab(ctx, dock, dock_id) {
        track.uid = Some(uid);
        track.size = size.or(track.size);
    }
    if track.uid.is_some() && pos.is_some() {
        track.pos = pos;
    }
    None
}

/// Floating dock windows of the main dock dragged out of the app, with their outer rects
/// in screen space. Floating windows left only partly off the app are moved back in.
///
/// egui_dock 0.21 never fills `WindowState::rect()` / `dragged()` (they stay
/// `Rect::NOTHING` / false), so the rect is read from the egui area behind the window.
fn take_outside_surfaces<T>(ctx: &egui::Context, main: &mut DockState<T>, content: Rect, inner: Rect) -> Vec<(Vec<T>, Rect)> {
    let (down, released, pointer) = ctx.input(|i| (i.pointer.primary_down(), i.pointer.primary_released(), i.pointer.latest_pos()));
    if down {
        return Vec::new(); // decide once the drag ends: no flicker mid-drag
    }
    let released_at = pointer.filter(|_| released);
    let windows: Vec<(SurfaceIndex, Rect)> = main
        .iter_surfaces_indexed()
        .filter(|(_, s)| matches!(s, Surface::Window(..)))
        .filter_map(|(i, _)| ctx.memory(|m| m.area_rect(floating_area_id(i))).map(|r| (i, r)))
        .collect();
    let mut out = Vec::new();
    // Highest index first, so removing a surface doesn't shift the ones still to visit.
    for (i, rect) in windows.into_iter().rev() {
        match geom::floating_verdict(rect, content, released_at) {
            geom::FloatVerdict::Keep => {}
            geom::FloatVerdict::PullBack(pos) => {
                if let Some(state) = main.get_window_state_mut(i) {
                    state.set_position(pos);
                }
            }
            geom::FloatVerdict::Detach => {
                let outer = match released_at {
                    // Dropped by hand: the native title bar lands under the pointer.
                    Some(p) => geom::spawn_rect(inner.min + p.to_vec2(), rect.size()),
                    None => rect.translate(inner.min.to_vec2()),
                };
                if let Some(Surface::Window(mut tree, _)) = main.remove_surface(i) {
                    out.push((tree_tabs(&mut tree), outer));
                }
            }
        }
    }
    out
}

/// The egui area id egui_dock gives the floating window of surface `i`
/// (`format!("window {surf_index:?}")` in its `show_window_surface`).
fn floating_area_id(i: SurfaceIndex) -> Id {
    Id::new(format!("window {i:?}"))
}

fn tree_tabs<T>(tree: &mut Tree<T>) -> Vec<T> {
    let mut out = Vec::new();
    for node in tree.iter_mut() {
        if let Node::Leaf(leaf) = node {
            out.append(&mut leaf.tabs);
        }
    }
    out
}

fn dock_tabs(mut dock: DockState<Tab>) -> Vec<Tab> {
    let mut out = Vec::new();
    for surface in dock.iter_surfaces_mut() {
        if let Some(tree) = surface.node_tree_mut() {
            out.append(&mut tree_tabs(tree));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use eframe::egui::{Event, PointerButton, RawInput, pos2};

    use super::*;

    const SCREEN: Vec2 = vec2(800.0, 600.0);
    /// Where the app's client area sits on the (virtual) desktop.
    const INNER_MIN: Pos2 = pos2(100.0, 50.0);

    struct Viewer;

    impl TabViewer for Viewer {
        type Tab = String;
        fn id(&mut self, tab: &mut String) -> Id {
            Id::new(tab.as_str())
        }
        fn title(&mut self, tab: &mut String) -> egui::WidgetText {
            tab.as_str().into()
        }
        fn ui(&mut self, ui: &mut egui::Ui, tab: &mut String) {
            ui.label(tab.as_str());
        }
    }

    /// One app frame: the dock with the app's floating-window bounds, then the detach pass.
    fn frame(ctx: &egui::Context, dock: &mut DockState<String>, events: Vec<Event>) -> Vec<(Vec<String>, Rect)> {
        let content = Rect::from_min_size(Pos2::ZERO, SCREEN);
        let raw = RawInput { screen_rect: Some(content), events, ..Default::default() };
        let mut out = Vec::new();
        let mut full = ctx.run_ui(raw, |ui| {
            DockArea::new(dock).window_bounds(geom::floating_bounds(ui.ctx().content_rect())).show_inside(ui, &mut Viewer);
            out = take_outside_surfaces(ui.ctx(), dock, content, Rect::from_min_size(INNER_MIN, SCREEN));
        });
        // No renderer consumes the font atlas here; dropping a non-empty delta panics.
        full.textures_delta.clear();
        out
    }

    fn dock_with_window(pos: Pos2) -> (DockState<String>, SurfaceIndex) {
        let mut dock = DockState::new(vec!["main".to_string()]);
        let i = dock.add_window(vec!["float".to_string()]);
        dock.get_window_state_mut(i).unwrap().set_position(pos).set_size(vec2(300.0, 200.0));
        (dock, i)
    }

    fn window_count(dock: &DockState<String>) -> usize {
        dock.iter_surfaces().filter(|s| matches!(s, Surface::Window(..))).count()
    }

    #[test]
    fn floating_window_reads_its_real_rect_and_stays_inside() {
        let ctx = egui::Context::default();
        let (mut dock, i) = dock_with_window(pos2(200.0, 150.0));
        for _ in 0..3 {
            assert!(frame(&ctx, &mut dock, vec![]).is_empty());
        }
        let rect = ctx.memory(|m| m.area_rect(floating_area_id(i))).expect("egui_dock's window area id changed");
        assert!((rect.min - pos2(200.0, 150.0)).length() < 1.0, "{rect:?}");
        assert_eq!(window_count(&dock), 1);
    }

    #[test]
    fn floating_window_moved_mostly_off_the_app_becomes_native() {
        let ctx = egui::Context::default();
        // Before the bounds fix egui clamped this back inside the app.
        let (mut dock, _) = dock_with_window(pos2(650.0, 150.0));
        let mut spawned = Vec::new();
        for _ in 0..3 {
            spawned.extend(frame(&ctx, &mut dock, vec![]));
        }
        assert_eq!(spawned.len(), 1, "exactly one native window");
        let (tabs, outer) = &spawned[0];
        assert_eq!(tabs, &vec!["float".to_string()]);
        assert!((outer.min - (INNER_MIN + vec2(650.0, 150.0))).length() < 1.0, "screen space: {outer:?}");
        assert_eq!(window_count(&dock), 0);
        assert_eq!(dock.iter_all_tabs().count(), 1);
    }

    #[test]
    fn floating_window_dragged_out_detaches_only_on_release() {
        let ctx = egui::Context::default();
        let (mut dock, i) = dock_with_window(pos2(300.0, 150.0));
        for _ in 0..3 {
            frame(&ctx, &mut dock, vec![]);
        }
        let rect = ctx.memory(|m| m.area_rect(floating_area_id(i))).unwrap();
        // Grab the empty end of the tab bar.
        let mut at = pos2(rect.max.x - 20.0, rect.min.y + 10.0);
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        frame(&ctx, &mut dock, vec![Event::PointerMoved(at)]);
        frame(&ctx, &mut dock, vec![button(at, true)]);
        for _ in 0..12 {
            at += vec2(50.0, 0.0);
            assert!(frame(&ctx, &mut dock, vec![Event::PointerMoved(at)]).is_empty(), "nothing happens mid-drag");
        }
        let moved = ctx.memory(|m| m.area_rect(floating_area_id(i))).unwrap();
        assert!(moved.min.x > rect.min.x + 300.0, "the window followed the pointer past the edge: {moved:?}");
        let spawned = frame(&ctx, &mut dock, vec![button(at, false)]);
        assert_eq!(spawned.len(), 1);
        assert!(spawned[0].1.contains(INNER_MIN + at.to_vec2()), "pointer on the new window's title bar");
        assert_eq!(window_count(&dock), 0);
    }
}
