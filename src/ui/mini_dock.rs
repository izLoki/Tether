//! Minimizable pop-up windows and a movable Mini-Dock Bar with a macOS Genie-style
//! funnel animation.
//!
//! Popped-out windows (in-app floating `Surface::Window` panes and detached OS windows)
//! can be minimized via their header `—` button, by clicking their chip in the mini-dock
//! bar, or by dragging them onto the mini-dock bar. The bar itself can be dragged and
//! snapped ("juntable") to any of the four window edges (Bottom, Top, Left, Right).

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Id, LayerId, Mesh, Order, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};
use egui_dock::{DockState, Node, Surface, SurfaceIndex};

use super::tabs::Tab;
use crate::layout::MiniBarSide;
use crate::settings;
use crate::theme;

const GENIE_SECS: f64 = 0.38;
const GENIE_SLICES: usize = 28;
const BAR_MARGIN: f32 = 10.0;
const ITEM_W_HORIZ: f32 = 148.0;
const ITEM_H_HORIZ: f32 = 34.0;
const ITEM_W_VERT: f32 = 42.0;
const ITEM_H_VERT: f32 = 42.0;
const HANDLE_THICK: f32 = 18.0;
const DEFAULT_FLOAT_SIZE: Vec2 = vec2(620.0, 420.0);

/// A popped-out window stored in the mini-dock bar while minimized.
pub struct MinimizedWindow {
    pub id: u64,
    pub tabs: Vec<Tab>,
    /// Last floating rect inside the main window before minimizing.
    pub restore_rect: Rect,
}

impl MinimizedWindow {
    pub fn primary_tab(&self) -> Option<&Tab> {
        self.tabs.first()
    }

    pub fn primary_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.first_mut()
    }
}

/// Active macOS Genie funnel animation between `window_rect` and `dock_rect`.
struct GenieAnim {
    window_rect: Rect,
    dock_rect: Rect,
    side: MiniBarSide,
    minimizing: bool,
    start_time: f64,
    hue: f32,
    icon: String,
    title: String,
}

/// Summary of an active (unminimized) floating `Surface::Window` in the main dock.
struct FloatingSurfaceInfo {
    surface: SurfaceIndex,
    rect: Rect,
    title: String,
    icon: String,
    hue: f32,
    status: Color32,
}

#[derive(Default)]
pub struct MiniDock {
    minimized: Vec<MinimizedWindow>,
    next_id: u64,
    anims: Vec<GenieAnim>,
    /// True while the user is dragging the mini-dock bar itself to reposition/snap it.
    dragging_bar: bool,
    /// Last computed screen rect of the mini-dock bar (in main window coordinates).
    last_bar_rect: Option<Rect>,
    /// Slot rects per minimized id (or surface index) for targeting the Genie animation.
    slot_rects: Vec<(u64, Rect)>,
    /// Whether a dragged floating window was over the mini-dock bar on the previous frame.
    window_hover_bar: Option<SurfaceIndex>,
}

impl MiniDock {
    pub fn is_empty(&self) -> bool {
        self.minimized.is_empty()
    }

    pub fn minimized_windows(&self) -> &[MinimizedWindow] {
        &self.minimized
    }

    /// Iterates every `Tab` currently minimized in the mini-dock bar (so SSH connections
    /// stay alive and lookups find them).
    pub fn tabs(&self) -> impl Iterator<Item = &Tab> {
        self.minimized.iter().flat_map(|w| w.tabs.iter())
    }

    pub fn tabs_mut(&mut self) -> impl Iterator<Item = &mut Tab> {
        self.minimized.iter_mut().flat_map(|w| w.tabs.iter_mut())
    }

    /// Removes and returns a minimized tab by `uid` (used when closing or focusing from
    /// the Sessions sidebar).
    pub fn remove_tab_by_uid(&mut self, uid: u64) -> Option<Tab> {
        let win_idx = self.minimized.iter().position(|w| w.tabs.iter().any(|t| t.uid == uid))?;
        let pos = self.minimized[win_idx].tabs.iter().position(|t| t.uid == uid)?;
        let tab = self.minimized[win_idx].tabs.remove(pos);
        if self.minimized[win_idx].tabs.is_empty() {
            self.minimized.remove(win_idx);
        }
        Some(tab)
    }

    /// Restores the minimized window containing `uid` into `dock` as a floating window,
    /// playing the Genie restore animation. Returns `true` if found and restored.
    pub fn restore_by_uid(&mut self, ctx: &egui::Context, dock: &mut DockState<Tab>, uid: u64) -> bool {
        let Some(idx) = self.minimized.iter().position(|w| w.tabs.iter().any(|t| t.uid == uid)) else {
            return false;
        };
        let win = self.minimized.remove(idx);
        self.restore_window_into(ctx, dock, win, None);
        true
    }

    /// Minimizes a set of tabs from `from_rect` (in main window coordinates) into the
    /// mini-dock bar and starts the macOS Genie animation.
    pub fn minimize_tabs(&mut self, ctx: &egui::Context, mut tabs: Vec<Tab>, from_rect: Rect) {
        if tabs.is_empty() {
            return;
        }
        self.next_id += 1;
        let id = self.next_id;
        let (title, icon, hue) = match tabs.first_mut() {
            Some(t) => (t.display_title(), t.icon().to_string(), t.profile.hue),
            None => ("Window".into(), "🗗".into(), 200.0),
        };
        let side = settings::snapshot().layout.mini_bar_side;
        let content = ctx.content_rect();
        let dock_rect = self.last_bar_rect.map(|r| Rect::from_center_size(r.center(), vec2(36.0, 28.0))).unwrap_or_else(|| default_dock_slot(content, side));
        let now = ctx.input(|i| i.time);
        self.anims.push(GenieAnim { window_rect: from_rect, dock_rect, side, minimizing: true, start_time: now, hue, icon, title });
        self.minimized.push(MinimizedWindow { id, tabs, restore_rect: from_rect });
        ctx.request_repaint();
    }

    /// Minimizes an in-app floating `Surface::Window` from `dock` into the mini-dock bar.
    pub fn minimize_surface(&mut self, ctx: &egui::Context, dock: &mut DockState<Tab>, surface: SurfaceIndex, rect: Rect) {
        if surface.is_main() {
            return;
        }
        if let Some(Surface::Window(mut tree, _)) = dock.remove_surface(surface) {
            let mut tabs = Vec::new();
            for node in tree.iter_mut() {
                if let Node::Leaf(leaf) = node {
                    tabs.append(&mut leaf.tabs);
                }
            }
            self.minimize_tabs(ctx, tabs, rect);
        }
    }

    fn restore_window_into(&mut self, ctx: &egui::Context, dock: &mut DockState<Tab>, mut win: MinimizedWindow, override_pos: Option<Pos2>) {
        let (title, icon, hue) = match win.tabs.first_mut() {
            Some(t) => (t.display_title(), t.icon().to_string(), t.profile.hue),
            None => ("Window".into(), "🗗".into(), 200.0),
        };
        let content = ctx.content_rect().shrink(12.0);
        let size = win.restore_rect.size().max(vec2(320.0, 220.0)).min(content.size());
        let pos = override_pos.unwrap_or(win.restore_rect.min);
        let clamped_pos = pos2(
            pos.x.clamp(content.left(), (content.right() - size.x).max(content.left())),
            pos.y.clamp(content.top(), (content.bottom() - size.y).max(content.top())),
        );
        let target_rect = Rect::from_min_size(clamped_pos, size);
        let side = settings::snapshot().layout.mini_bar_side;
        let dock_rect = self
            .slot_rects
            .iter()
            .find(|(id, _)| *id == win.id)
            .map(|(_, r)| *r)
            .or_else(|| self.last_bar_rect.map(|r| Rect::from_center_size(r.center(), vec2(36.0, 28.0))))
            .unwrap_or_else(|| default_dock_slot(ctx.content_rect(), side));
        let now = ctx.input(|i| i.time);
        self.anims.push(GenieAnim { window_rect: target_rect, dock_rect, side, minimizing: false, start_time: now, hue, icon, title });
        let surf = dock.add_window(win.tabs);
        if let Some(state) = dock.get_window_state_mut(surf) {
            state.set_position(target_rect.min).set_size(target_rect.size());
        }
        ctx.request_repaint();
    }

    /// Renders header buttons (`—` minimize and `✕` close) on every floating
    /// `Surface::Window`, handles dragging floating windows into the mini-dock bar,
    /// draws the movable mini-dock bar, and paints active macOS Genie animations.
    pub fn frame(&mut self, ctx: &egui::Context, dock: &mut DockState<Tab>, central_rect: Rect) {
        let floating = collect_floating_surfaces(ctx, dock);
        self.paint_floating_window_controls(ctx, dock, &floating);
        self.detect_drag_to_minimize(ctx, dock, &floating);
        self.show_bar(ctx, dock, &floating, central_rect);
        self.paint_genie_animations(ctx);
    }

    /// Paints explicit `—` (Minimize) and `✕` (Close) buttons in the top-right header
    /// of every floating `Surface::Window` so they can always be minimized or closed
    /// even when `TabPosition::Sidebar` is active.
    fn paint_floating_window_controls(&mut self, ctx: &egui::Context, dock: &mut DockState<Tab>, floating: &[FloatingSurfaceInfo]) {
        let mut to_minimize = Vec::new();
        let mut to_close = Vec::new();
        for info in floating {
            let area_id = Id::new(("tether_float_ctrl", info.surface));
            let header_h = 28.0;
            let ctrl_size = vec2(52.0, 22.0);
            let ctrl_pos = pos2(info.rect.right() - ctrl_size.x - 6.0, info.rect.top() + (header_h - ctrl_size.y) * 0.5);
            egui::Area::new(area_id).order(Order::Foreground).fixed_pos(ctrl_pos).show(ctx, |ui| {
                let pill = Rect::from_min_size(ctrl_pos, ctrl_size);
                ui.painter().rect_filled(pill, CornerRadius::same(6), theme::surface_2().gamma_multiply(0.92));
                ui.painter().rect_stroke(pill, CornerRadius::same(6), Stroke::new(1.0, theme::border().gamma_multiply(0.5)), StrokeKind::Inside);
                let min_rect = Rect::from_min_size(pill.min + vec2(3.0, 2.0), vec2(21.0, 18.0));
                let close_rect = Rect::from_min_size(pill.min + vec2(27.0, 2.0), vec2(21.0, 18.0));

                let min_resp = ui.interact(min_rect, area_id.with("min"), Sense::click()).on_hover_text("Minimize to mini-bar");
                if min_resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    ui.painter().rect_filled(min_rect, CornerRadius::same(4), theme::accent_soft());
                }
                let min_col = if min_resp.hovered() { theme::accent() } else { theme::text_dim() };
                let mc = min_rect.center();
                ui.painter().line_segment([mc + vec2(-4.5, 1.0), mc + vec2(4.5, 1.0)], Stroke::new(1.6, min_col));
                if min_resp.clicked() {
                    to_minimize.push((info.surface, info.rect));
                }

                let close_resp = ui.interact(close_rect, area_id.with("close"), Sense::click()).on_hover_text("Close window");
                if close_resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    ui.painter().rect_filled(close_rect, CornerRadius::same(4), theme::danger().gamma_multiply(0.24));
                }
                let close_col = if close_resp.hovered() { theme::danger() } else { theme::text_dim() };
                let cc = close_rect.center();
                let d = 3.5;
                ui.painter().line_segment([cc + vec2(-d, -d), cc + vec2(d, d)], Stroke::new(1.5, close_col));
                ui.painter().line_segment([cc + vec2(d, -d), cc + vec2(-d, d)], Stroke::new(1.5, close_col));
                if close_resp.clicked() {
                    to_close.push(info.surface);
                }
            });
        }
        for (surf, rect) in to_minimize {
            self.minimize_surface(ctx, dock, surf, rect);
        }
        for surf in to_close.into_iter().rev() {
            if let Some(Surface::Window(mut tree, _)) = dock.remove_surface(surf) {
                for node in tree.iter_mut() {
                    if let Node::Leaf(leaf) = node {
                        for mut tab in leaf.tabs.drain(..) {
                            if let super::tabs::TabBody::Terminal(t) = &mut tab.body {
                                t.handle.close();
                            }
                        }
                    }
                }
            }
        }
    }

    /// Detects when the user drags a floating window onto the mini-dock bar (or near the
    /// mini-dock bar's drop zone) and minimizes it on release with the Genie animation.
    fn detect_drag_to_minimize(&mut self, ctx: &egui::Context, dock: &mut DockState<Tab>, floating: &[FloatingSurfaceInfo]) {
        let Some(bar_rect) = self.last_bar_rect else {
            self.window_hover_bar = None;
            return;
        };
        let drop_zone = bar_rect.expand(24.0);
        let (down, released, pointer) = ctx.input(|i| (i.pointer.primary_down(), i.pointer.primary_released(), i.pointer.latest_pos()));
        let Some(pos) = pointer else {
            self.window_hover_bar = None;
            return;
        };

        if down && !self.dragging_bar {
            // Check if pointer is dragging a floating window's title bar over the mini-dock bar.
            let dragged_surf = floating.iter().find(|f| drop_zone.contains(pos) && f.rect.expand(12.0).contains(pos)).map(|f| f.surface);
            self.window_hover_bar = dragged_surf;
            if dragged_surf.is_some() {
                let p = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("mini_dock_drop_glow")));
                p.rect_filled(bar_rect.expand(4.0), CornerRadius::same(12), theme::accent().gamma_multiply(0.18));
                p.rect_stroke(bar_rect.expand(4.0), CornerRadius::same(12), Stroke::new(2.0, theme::accent()), StrokeKind::Inside);
            }
        } else if released {
            if let Some(surf) = self.window_hover_bar.take()
                && drop_zone.contains(pos)
                && let Some(info) = floating.iter().find(|f| f.surface == surf)
            {
                self.minimize_surface(ctx, dock, surf, info.rect);
            }
        } else {
            self.window_hover_bar = None;
        }
    }

    fn show_bar(&mut self, ctx: &egui::Context, dock: &mut DockState<Tab>, floating: &[FloatingSurfaceInfo], central_rect: Rect) {
        let total_items = self.minimized.len() + floating.len();
        if total_items == 0 && !self.dragging_bar && self.anims.is_empty() {
            self.last_bar_rect = None;
            return;
        }

        let bounds = if central_rect.width() > 100.0 && central_rect.height() > 100.0 { central_rect } else { ctx.content_rect() };
        let layout = &settings::snapshot().layout;
        let side = layout.mini_bar_side;
        let offset = layout.mini_bar_offset;
        let horiz = side.is_horizontal();

        let item_count = total_items.max(1) as f32;
        let bar_size = if horiz {
            vec2(HANDLE_THICK + item_count * (ITEM_W_HORIZ + 6.0) + 10.0, ITEM_H_HORIZ + 10.0)
        } else {
            vec2(ITEM_W_VERT + 10.0, HANDLE_THICK + item_count * (ITEM_H_VERT + 6.0) + 10.0)
        };
        let bar_rect = compute_bar_rect(bounds, side, offset, bar_size);
        self.last_bar_rect = Some(bar_rect);
        self.slot_rects.clear();

        let mut restore_req: Option<(usize, Option<Pos2>)> = None;
        let mut close_min_req: Option<usize> = None;
        let mut toggle_float_req: Option<(SurfaceIndex, Rect)> = None;

        let area_id = Id::new("tether_mini_dock_bar");
        egui::Area::new(area_id).order(Order::Foreground).fixed_pos(bar_rect.min).show(ctx, |ui| {
            let p = ui.painter();
            p.add(egui::Shadow { offset: [0, 4], blur: 14, spread: 0, color: theme::shadow() }.as_shape(bar_rect, CornerRadius::same(12)));
            p.rect_filled(bar_rect, CornerRadius::same(12), theme::surface_2().gamma_multiply(0.96));
            p.rect_stroke(bar_rect, CornerRadius::same(12), Stroke::new(1.0, theme::border()), StrokeKind::Inside);

            // 1. Grip handle for dragging the mini-dock bar to any of the 4 sides.
            let handle_rect = if horiz {
                Rect::from_min_size(bar_rect.min + vec2(4.0, 4.0), vec2(HANDLE_THICK, bar_rect.height() - 8.0))
            } else {
                Rect::from_min_size(bar_rect.min + vec2(4.0, 4.0), vec2(bar_rect.width() - 8.0, HANDLE_THICK))
            };
            let handle_resp = ui
                .interact(handle_rect, area_id.with("grip"), Sense::click_and_drag())
                .on_hover_text("Drag to move & snap the mini-bar (Bottom, Top, Left, Right)");
            if handle_resp.hovered() || handle_resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            }
            paint_grip(p, handle_rect, horiz, handle_resp.hovered() || self.dragging_bar);
            handle_resp.context_menu(|ui| {
                ui.label(egui::RichText::new("Mini-bar position").size(11.0).color(theme::text_faint()));
                for s in MiniBarSide::ALL {
                    if ui.radio(side == s, s.label()).clicked() {
                        let _ = settings::update(|st| st.layout.mini_bar_side = s);
                        ui.close();
                    }
                }
            });

            if handle_resp.drag_started() {
                self.dragging_bar = true;
            }
            if self.dragging_bar {
                if let Some(ptr) = ui.ctx().pointer_latest_pos() {
                    let (new_side, new_offset) = snap_bar_to_edge(bounds, ptr);
                    paint_edge_snap_preview(ui.ctx(), bounds, new_side);
                    settings::preview(|st| {
                        st.layout.mini_bar_side = new_side;
                        st.layout.mini_bar_offset = new_offset;
                    });
                }
                if ui.input(|i| i.pointer.any_released()) {
                    self.dragging_bar = false;
                    let _ = settings::save();
                }
            }

            // 2. Render items (minimized windows first, then active floating windows).
            let mut cursor =
                if horiz { pos2(handle_rect.right() + 4.0, bar_rect.top() + 5.0) } else { pos2(bar_rect.left() + 5.0, handle_rect.bottom() + 4.0) };
            let step = if horiz { vec2(ITEM_W_HORIZ + 6.0, 0.0) } else { vec2(0.0, ITEM_H_VERT + 6.0) };
            let item_size = if horiz { vec2(ITEM_W_HORIZ, ITEM_H_HORIZ) } else { vec2(ITEM_W_VERT, ITEM_H_VERT) };

            for (idx, win) in self.minimized.iter_mut().enumerate() {
                let slot = Rect::from_min_size(cursor, item_size);
                cursor += step;
                self.slot_rects.push((win.id, slot));

                let (title, icon, hue, status) = match win.primary_tab_mut() {
                    Some(tab) => (tab.display_title(), tab.icon().to_string(), tab.profile.hue, tab.status_color()),
                    None => ("Window".into(), "🗗".into(), 200.0, theme::accent()),
                };
                let item_id = area_id.with(("min_item", win.id));
                let resp = ui.interact(slot, item_id, Sense::click_and_drag());
                let hover = ui.ctx().animate_bool_with_time(item_id.with("h"), resp.hovered(), theme::anim_time(0.12));

                paint_bar_chip(ui, slot, horiz, &icon, &title, hue, status, true, hover);

                // Close button on hover for minimized window.
                let close_center = if horiz { pos2(slot.right() - 12.0, slot.center().y) } else { pos2(slot.right() - 7.0, slot.top() + 7.0) };
                let mut closed_clicked = false;
                if hover > 0.1 {
                    let cr = Rect::from_center_size(close_center, vec2(16.0, 16.0));
                    let c_resp = ui.interact(cr, item_id.with("close"), Sense::click()).on_hover_text("Close minimized window");
                    if c_resp.hovered() {
                        ui.painter().rect_filled(cr, CornerRadius::same(4), theme::danger().gamma_multiply(0.28));
                    }
                    let col = if c_resp.hovered() { theme::danger() } else { theme::text_dim() };
                    let d = 3.0;
                    ui.painter().line_segment([close_center + vec2(-d, -d), close_center + vec2(d, d)], Stroke::new(1.3, col));
                    ui.painter().line_segment([close_center + vec2(d, -d), close_center + vec2(-d, d)], Stroke::new(1.3, col));
                    if c_resp.clicked() {
                        closed_clicked = true;
                        close_min_req = Some(idx);
                    }
                }

                if !closed_clicked {
                    if resp.clicked() {
                        restore_req = Some((idx, None));
                    } else if resp.dragged()
                        && let Some(ptr) = ui.ctx().pointer_latest_pos()
                        && !bar_rect.expand(18.0).contains(ptr)
                    {
                        restore_req = Some((idx, Some(ptr - vec2(120.0, 16.0))));
                    }
                }
                resp.on_hover_text(format!("{title} (minimized — click or drag out to restore)"));
            }

            for info in floating {
                let slot = Rect::from_min_size(cursor, item_size);
                cursor += step;
                let item_id = area_id.with(("float_item", info.surface));
                let resp = ui.interact(slot, item_id, Sense::click());
                let hover = ui.ctx().animate_bool_with_time(item_id.with("h"), resp.hovered(), theme::anim_time(0.12));

                paint_bar_chip(ui, slot, horiz, &info.icon, &info.title, info.hue, info.status, false, hover);
                if resp.clicked() {
                    toggle_float_req = Some((info.surface, info.rect));
                }
                resp.on_hover_text(format!("{} (open — click to minimize)", info.title));
            }
        });

        if let Some(idx) = close_min_req {
            let mut win = self.minimized.remove(idx);
            for mut tab in win.tabs.drain(..) {
                if let super::tabs::TabBody::Terminal(t) = &mut tab.body {
                    t.handle.close();
                }
            }
        } else if let Some((idx, pos)) = restore_req {
            let win = self.minimized.remove(idx);
            self.restore_window_into(ctx, dock, win, pos);
        } else if let Some((surf, rect)) = toggle_float_req {
            self.minimize_surface(ctx, dock, surf, rect);
        }
    }

    /// Renders the curved macOS Genie funnel animation for any minimizing or restoring windows.
    fn paint_genie_animations(&mut self, ctx: &egui::Context) {
        if self.anims.is_empty() {
            return;
        }
        let now = ctx.input(|i| i.time);
        let duration = theme::anim_time(GENIE_SECS as f32) as f64;
        let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("tether_genie_anim")));
        let mut keep = Vec::with_capacity(self.anims.len());

        for anim in self.anims.drain(..) {
            let raw_t = if duration <= 0.001 { 1.0 } else { ((now - anim.start_time) / duration).clamp(0.0, 1.0) as f32 };
            if raw_t < 1.0 {
                paint_genie_funnel(&painter, &anim, raw_t);
                keep.push(anim);
            }
        }
        self.anims = keep;
        if !self.anims.is_empty() {
            ctx.request_repaint();
        }
    }
}

/// Paints a single frame of the macOS Genie funnel effect between `anim.window_rect`
/// and `anim.dock_rect`.
fn paint_genie_funnel(painter: &Painter, anim: &GenieAnim, raw_t: f32) {
    // `p` is 0.0 at full window and 1.0 when fully inside the dock slot.
    let p = if anim.minimizing { raw_t } else { 1.0 - raw_t };
    let pinch = smoothstep(0.0, 0.55, p);
    let slide = smoothstep(0.20, 1.0, p);

    let tint = theme::hue_color(anim.hue);
    let base_fill = theme::surface_2();
    let header_fill = tint.gamma_multiply(0.32).lerp_to_gamma(base_fill, 0.5);
    let stroke_col = tint.gamma_multiply(0.85 * (1.0 - p * 0.4));

    let horiz_bar = anim.side.is_horizontal();
    let mut mesh = Mesh::default();
    let mut left_edge = Vec::with_capacity(GENIE_SLICES + 1);
    let mut right_edge = Vec::with_capacity(GENIE_SLICES + 1);

    for i in 0..=GENIE_SLICES {
        // `s` = 0.0 at the end nearest the dock, `1.0` at the end farthest from the dock.
        let s = i as f32 / GENIE_SLICES as f32;
        let u = (s - slide).max(0.0);
        // S-curve funnel profile: near the dock (u=0) it pinches tightly to the dock slot;
        // toward the far end (u=1) it flares out to the window width until `slide` pulls it in.
        let flare = smoothstep(0.0, 1.0, u) * (1.0 - slide * 0.85);
        let curve = 1.0 - pinch * (1.0 - flare);

        if horiz_bar {
            let (dock_y, win_far_y) = match anim.side {
                MiniBarSide::Bottom => (anim.dock_rect.center().y, anim.window_rect.top()),
                MiniBarSide::Top => (anim.dock_rect.center().y, anim.window_rect.bottom()),
                _ => unreachable!(),
            };
            let win_near_y = match anim.side {
                MiniBarSide::Bottom => anim.window_rect.bottom(),
                MiniBarSide::Top => anim.window_rect.top(),
                _ => unreachable!(),
            };
            let near_y = lerp(win_near_y, dock_y, pinch);
            let far_y = lerp(win_far_y, dock_y, slide);
            let y = lerp(near_y, far_y, s);

            let x0 = lerp(anim.dock_rect.left(), anim.window_rect.left(), curve);
            let x1 = lerp(anim.dock_rect.right(), anim.window_rect.right(), curve);
            let col = if s > 0.85 { header_fill } else { base_fill }.gamma_multiply(1.0 - 0.25 * p);
            let base_idx = mesh.vertices.len() as u32;
            mesh.colored_vertex(pos2(x0, y), col);
            mesh.colored_vertex(pos2(x1, y), col);
            left_edge.push(pos2(x0, y));
            right_edge.push(pos2(x1, y));
            if i > 0 {
                mesh.add_triangle(base_idx - 2, base_idx - 1, base_idx);
                mesh.add_triangle(base_idx - 1, base_idx + 1, base_idx);
            }
        } else {
            let (dock_x, win_far_x) = match anim.side {
                MiniBarSide::Right => (anim.dock_rect.center().x, anim.window_rect.left()),
                MiniBarSide::Left => (anim.dock_rect.center().x, anim.window_rect.right()),
                _ => unreachable!(),
            };
            let win_near_x = match anim.side {
                MiniBarSide::Right => anim.window_rect.right(),
                MiniBarSide::Left => anim.window_rect.left(),
                _ => unreachable!(),
            };
            let near_x = lerp(win_near_x, dock_x, pinch);
            let far_x = lerp(win_far_x, dock_x, slide);
            let x = lerp(near_x, far_x, s);

            let y0 = lerp(anim.dock_rect.top(), anim.window_rect.top(), curve);
            let y1 = lerp(anim.dock_rect.bottom(), anim.window_rect.bottom(), curve);
            let col = if s > 0.85 { header_fill } else { base_fill }.gamma_multiply(1.0 - 0.25 * p);
            let base_idx = mesh.vertices.len() as u32;
            mesh.colored_vertex(pos2(x, y0), col);
            mesh.colored_vertex(pos2(x, y1), col);
            left_edge.push(pos2(x, y0));
            right_edge.push(pos2(x, y1));
            if i > 0 {
                mesh.add_triangle(base_idx - 2, base_idx - 1, base_idx);
                mesh.add_triangle(base_idx - 1, base_idx + 1, base_idx);
            }
        }
    }

    painter.add(egui::Shape::mesh(mesh));
    painter.add(egui::Shape::line(left_edge.clone(), Stroke::new(1.6, stroke_col)));
    painter.add(egui::Shape::line(right_edge.clone(), Stroke::new(1.6, stroke_col)));

    if p < 0.72
        && let (Some(l), Some(r)) = (left_edge.last(), right_edge.last())
    {
        let center = pos2((l.x + r.x) * 0.5, (l.y + r.y) * 0.5);
        let alpha = (1.0 - p / 0.72).clamp(0.0, 1.0);
        let label = format!("{}  {}", anim.icon, anim.title);
        painter.text(center, Align2::CENTER_CENTER, label, FontId::proportional(11.5 * (1.0 - 0.3 * p)), theme::text().gamma_multiply(alpha));
    }
}

fn collect_floating_surfaces(ctx: &egui::Context, dock: &mut DockState<Tab>) -> Vec<FloatingSurfaceInfo> {
    let indices: Vec<SurfaceIndex> = dock.iter_surfaces_indexed().filter_map(|(i, s)| matches!(s, Surface::Window(..)).then_some(i)).collect();
    let mut out = Vec::new();
    for i in indices {
        let area_id = Id::new(format!("window {i:?}"));
        let Some(rect) = ctx.memory(|m| m.area_rect(area_id)) else { continue };
        let Some(Surface::Window(tree, _)) = dock.get_surface_mut(i) else { continue };
        let mut first_info = None;
        for node in tree.iter_mut() {
            if let Node::Leaf(leaf) = node {
                let active_idx = leaf.active.0.min(leaf.tabs.len().saturating_sub(1));
                if let Some(tab) = leaf.tabs.get_mut(active_idx) {
                    first_info = Some((tab.display_title(), tab.icon().to_string(), tab.profile.hue, tab.status_color()));
                    break;
                }
            }
        }
        if let Some((title, icon, hue, status)) = first_info {
            out.push(FloatingSurfaceInfo { surface: i, rect, title, icon, hue, status });
        }
    }
    out
}

fn compute_bar_rect(bounds: Rect, side: MiniBarSide, offset: f32, size: Vec2) -> Rect {
    let offset = offset.clamp(0.05, 0.95);
    match side {
        MiniBarSide::Bottom => {
            let avail_w = (bounds.width() - size.x - BAR_MARGIN * 2.0).max(0.0);
            let x = bounds.left() + BAR_MARGIN + avail_w * offset;
            let y = bounds.bottom() - size.y - BAR_MARGIN;
            Rect::from_min_size(pos2(x, y), size)
        }
        MiniBarSide::Top => {
            let avail_w = (bounds.width() - size.x - BAR_MARGIN * 2.0).max(0.0);
            let x = bounds.left() + BAR_MARGIN + avail_w * offset;
            let y = bounds.top() + BAR_MARGIN;
            Rect::from_min_size(pos2(x, y), size)
        }
        MiniBarSide::Left => {
            let avail_h = (bounds.height() - size.y - BAR_MARGIN * 2.0).max(0.0);
            let x = bounds.left() + BAR_MARGIN;
            let y = bounds.top() + BAR_MARGIN + avail_h * offset;
            Rect::from_min_size(pos2(x, y), size)
        }
        MiniBarSide::Right => {
            let avail_h = (bounds.height() - size.y - BAR_MARGIN * 2.0).max(0.0);
            let x = bounds.right() - size.x - BAR_MARGIN;
            let y = bounds.top() + BAR_MARGIN + avail_h * offset;
            Rect::from_min_size(pos2(x, y), size)
        }
    }
}

fn default_dock_slot(bounds: Rect, side: MiniBarSide) -> Rect {
    compute_bar_rect(bounds, side, 0.5, vec2(48.0, 34.0))
}

/// Determines which of the 4 window edges `ptr` is closest to, and the normalized offset
/// along that edge.
pub fn snap_bar_to_edge(bounds: Rect, ptr: Pos2) -> (MiniBarSide, f32) {
    let d_bottom = (bounds.bottom() - ptr.y).abs();
    let d_top = (ptr.y - bounds.top()).abs();
    let d_left = (ptr.x - bounds.left()).abs();
    let d_right = (bounds.right() - ptr.x).abs();

    let min_d = d_bottom.min(d_top).min(d_left).min(d_right);
    let side = if (min_d - d_bottom).abs() < 0.5 {
        MiniBarSide::Bottom
    } else if (min_d - d_top).abs() < 0.5 {
        MiniBarSide::Top
    } else if (min_d - d_left).abs() < 0.5 {
        MiniBarSide::Left
    } else {
        MiniBarSide::Right
    };

    let offset = if side.is_horizontal() {
        ((ptr.x - bounds.left()) / bounds.width().max(1.0)).clamp(0.05, 0.95)
    } else {
        ((ptr.y - bounds.top()) / bounds.height().max(1.0)).clamp(0.05, 0.95)
    };
    (side, offset)
}

fn paint_edge_snap_preview(ctx: &egui::Context, bounds: Rect, side: MiniBarSide) {
    let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("mini_dock_snap_preview")));
    let strip = match side {
        MiniBarSide::Bottom => Rect::from_min_max(pos2(bounds.left() + 24.0, bounds.bottom() - 8.0), pos2(bounds.right() - 24.0, bounds.bottom() - 2.0)),
        MiniBarSide::Top => Rect::from_min_max(pos2(bounds.left() + 24.0, bounds.top() + 2.0), pos2(bounds.right() - 24.0, bounds.top() + 8.0)),
        MiniBarSide::Left => Rect::from_min_max(pos2(bounds.left() + 2.0, bounds.top() + 24.0), pos2(bounds.left() + 8.0, bounds.bottom() - 24.0)),
        MiniBarSide::Right => Rect::from_min_max(pos2(bounds.right() - 8.0, bounds.top() + 24.0), pos2(bounds.right() - 2.0, bounds.bottom() - 24.0)),
    };
    painter.rect_filled(strip, CornerRadius::same(3), theme::accent().gamma_multiply(0.65));
}

fn paint_grip(p: &Painter, rect: Rect, horiz: bool, hot: bool) {
    let col = if hot { theme::accent() } else { theme::text_faint() };
    let c = rect.center();
    if horiz {
        for dy in [-5.0, 0.0, 5.0] {
            for dx in [-2.0, 2.0] {
                p.circle_filled(c + vec2(dx, dy), 1.3, col);
            }
        }
    } else {
        for dx in [-5.0, 0.0, 5.0] {
            for dy in [-2.0, 2.0] {
                p.circle_filled(c + vec2(dx, dy), 1.3, col);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_bar_chip(ui: &egui::Ui, slot: Rect, horiz: bool, icon: &str, title: &str, hue: f32, status: Color32, minimized: bool, hover: f32) {
    let p = ui.painter();
    let tint = theme::hue_color(hue);
    let bg = if minimized {
        theme::surface_3().lerp_to_gamma(theme::accent_soft(), hover * 0.5)
    } else {
        theme::surface().lerp_to_gamma(theme::surface_3(), hover * 0.5)
    };
    p.rect_filled(slot, CornerRadius::same(8), bg);
    let border_col = if minimized { tint.gamma_multiply(0.45 + 0.35 * hover) } else { theme::border() };
    p.rect_stroke(slot, CornerRadius::same(8), Stroke::new(1.0, border_col), StrokeKind::Inside);

    if horiz {
        let tile = Rect::from_center_size(pos2(slot.left() + 16.0, slot.center().y), vec2(22.0, 22.0));
        p.rect_filled(tile, CornerRadius::same(6), tint.gamma_multiply(0.22));
        p.text(tile.center(), Align2::CENTER_CENTER, icon, FontId::proportional(11.5), tint);
        p.circle_filled(pos2(tile.right() - 1.0, tile.bottom() - 1.0), 3.0, status);

        let text_max_w = (slot.width() - 48.0).max(20.0);
        let short: String = title.chars().take(16).collect();
        let galley = p.layout_no_wrap(short, FontId::proportional(12.0), theme::text());
        let text_pos = pos2(tile.right() + 7.0, slot.center().y - galley.size().y * 0.5);
        p.with_clip_rect(Rect::from_min_size(pos2(tile.right() + 4.0, slot.top()), vec2(text_max_w, slot.height()))).galley(text_pos, galley, theme::text());
    } else {
        let tile = Rect::from_center_size(slot.center(), vec2(28.0, 28.0));
        p.rect_filled(tile, CornerRadius::same(7), tint.gamma_multiply(0.24));
        p.text(tile.center(), Align2::CENTER_CENTER, icon, FontId::proportional(13.0), tint);
        p.circle_filled(pos2(tile.right() - 1.0, tile.bottom() - 1.0), 3.5, status);
    }

    // Active indicator dot on the outer edge (like macOS Dock).
    if !minimized {
        let dot = if horiz { pos2(slot.center().x, slot.bottom() - 2.0) } else { pos2(slot.left() + 2.0, slot.center().y) };
        p.circle_filled(dot, 2.2, theme::accent());
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0).max(1e-5)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Default spawn rect for a newly popped-out in-app floating window.
pub fn default_floating_rect(bounds: Rect) -> Rect {
    let size = DEFAULT_FLOAT_SIZE.min(bounds.size() * 0.82);
    let pos = bounds.center() - size * 0.5;
    Rect::from_min_size(pos, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_bar_picks_closest_edge_and_clamps_offset() {
        let bounds = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 600.0));
        let (side, off) = snap_bar_to_edge(bounds, pos2(500.0, 590.0));
        assert_eq!(side, MiniBarSide::Bottom);
        assert!((off - 0.5).abs() < 0.02);

        let (side, _) = snap_bar_to_edge(bounds, pos2(500.0, 8.0));
        assert_eq!(side, MiniBarSide::Top);

        let (side, _) = snap_bar_to_edge(bounds, pos2(12.0, 300.0));
        assert_eq!(side, MiniBarSide::Left);

        let (side, _) = snap_bar_to_edge(bounds, pos2(990.0, 300.0));
        assert_eq!(side, MiniBarSide::Right);
    }
}
