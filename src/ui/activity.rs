//! Activity strips (VS Code style): the icons of a view container, as a vertical strip
//! on a side bar's outer edge, a row along its top, or the panel's tab row. Icons switch
//! views on click, fold the container when clicked again, reorder or move to another
//! container by drag and drop (with drop zones on the window edges while dragging), and
//! offer "Move to" / "Hide" on right-click. The model is `crate::views`.

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Id, LayerId, Order, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use super::settings_widgets::{Icon, paint_icon};
use crate::keybindings::{self, Action};
use crate::layout::{BarOrientation, Container, ContainerState, LayoutSettings, Side, View};
use crate::theme;

/// Width of a vertical strip.
pub const STRIP_W: f32 = 48.0;
/// Height of a side bar's top icon row.
pub const ROW_H: f32 = 36.0;
/// Height of the panel's tab row.
pub const TABS_H: f32 = 34.0;
const ITEM: f32 = 40.0;
const GLYPH: f32 = 20.0;
const ROW_ITEM: f32 = 32.0;
const ROW_GLYPH: f32 = 16.0;
const GAP: f32 = 4.0;

/// Drag-and-drop payload of an activity icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewDrag(pub View);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewEvent {
    Click(View),
    /// Into `to` before the view now at `index` (`None` = the end).
    Move {
        view: View,
        to: Container,
        index: Option<usize>,
    },
    Hide(View),
    Reveal(View),
    Toggle(Container),
    SetBar(Container, BarOrientation),
    Reset,
    OpenSettings,
    LaunchAgent(crate::agents::AgentKind),
    CustomBoot(crate::agents::AgentKind),
}

/// A small count on an icon; `tone` (the colour of an agent status) makes it stand out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Badge {
    pub count: usize,
    pub tone: Option<Color32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripStyle {
    /// Big icons top to bottom (a side bar's outer edge).
    Vertical,
    /// Small icons left to right (a side bar's top).
    Row,
    /// Icon + title tabs left to right (the panel).
    Tabs,
}

/// The egui id of a container's resizable panel.
pub fn container_id(c: Container) -> Id {
    Id::new(("view_bar", c))
}

/// Drops the panel sizes egui remembers, so the settings' sizes apply again (after a
/// layout reset).
pub fn forget_sizes(ctx: &egui::Context) {
    ctx.data_mut(|d| {
        for c in Container::ALL {
            d.remove::<egui::PanelState>(container_id(c));
        }
    });
}

pub fn view_icon(v: View) -> Icon {
    match v {
        View::Hosts => Icon::Server,
        View::Sessions => Icon::Rows,
        View::Workspaces => Icon::Layout,
        View::Agents | View::Codex => Icon::Sparkle,
    }
}

/// Paints `v`'s icon in `rect` with `col`. Agents uses the agent mark of the tab rows
/// (in its own colour, faded the way `col` is relative to full text).
pub fn paint_view_icon(p: &egui::Painter, rect: Rect, v: View, col: Color32) {
    if let Some(kind) = v.agent() {
        let alpha = (f32::from(col.a()) / 255.0).clamp(0.35, 1.0);
        let strength = if col == theme::text() { 1.0 } else { 0.7 };
        super::agent_icons::paint(p, rect.shrink(rect.width() * 0.05), kind, alpha * strength);
    } else {
        paint_icon(p, rect, view_icon(v), col);
    }
}

/// The shortcut that brings `v` up (shown in its tooltip).
pub fn view_action(v: View) -> Action {
    match v {
        View::Sessions => Action::FocusSessionsView,
        View::Hosts => Action::SearchHosts,
        View::Workspaces => Action::FocusWorkspacesView,
        View::Agents => Action::ToggleAgents,
        View::Codex => Action::ToggleCodex,
    }
}

pub struct Strip<'a> {
    pub container: Container,
    pub state: &'a ContainerState,
    pub hidden: &'a [View],
    pub style: StripStyle,
    /// Vertical strips: the active marker hugs this edge (the window's).
    pub marker_right: bool,
    pub badge: &'a dyn Fn(View) -> Option<Badge>,
}

impl Strip<'_> {
    /// Lays the icons out in `rect`, registers it as a drop target and reports clicks,
    /// drags and menu picks.
    pub fn show(&self, ui: &mut Ui, rect: Rect, targets: &mut DropTargets, events: &mut Vec<ViewEvent>) {
        // The background first, so icons sit above it for hover and clicks.
        let bg = ui.interact(rect, Id::new(("view_strip", self.container)), Sense::click());
        bg.context_menu(|ui| self.strip_menu(ui, None, events));
        let vertical = self.style == StripStyle::Vertical;
        let mut items = Vec::with_capacity(self.state.views.len());
        let mut cursor = if vertical { rect.top() + 8.0 } else { rect.left() + 8.0 };
        for &v in &self.state.views {
            let r = match self.style {
                StripStyle::Vertical => Rect::from_min_size(Pos2::new(rect.center().x - ITEM / 2.0, cursor), Vec2::splat(ITEM)),
                StripStyle::Row => Rect::from_min_size(Pos2::new(cursor, rect.center().y - ROW_ITEM / 2.0), Vec2::splat(ROW_ITEM)),
                StripStyle::Tabs => Rect::from_min_size(Pos2::new(cursor, rect.top()), Vec2::new(tab_width(ui, v, (self.badge)(v)), rect.height())),
            };
            cursor += if vertical { r.height() } else { r.width() } + GAP;
            self.item(ui, v, r, rect, events);
            items.push(r);
        }
        targets.strips.push(StripTarget { container: self.container, rect, vertical, items, views: self.state.views.clone() });
    }

    fn item(&self, ui: &mut Ui, v: View, r: Rect, strip: Rect, events: &mut Vec<ViewEvent>) {
        let ctx = ui.ctx().clone();
        let resp = ui.interact(r, Id::new(("view_icon", v)), Sense::click_and_drag());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{} view", v.title())));
        let dragging = egui::DragAndDrop::payload::<ViewDrag>(&ctx).is_some();
        if resp.drag_started() {
            resp.dnd_set_drag_payload(ViewDrag(v));
        }
        let lifted = egui::DragAndDrop::payload::<ViewDrag>(&ctx).is_some_and(|d| d.0 == v);
        let selected = self.state.shown() == Some(v);
        let h = ctx.animate_bool_with_time(resp.id.with("h"), resp.hovered() && !dragging, theme::anim_time(0.1));
        let s = ctx.animate_bool_with_time(resp.id.with("s"), selected, theme::anim_time(0.16));
        if resp.hovered() && !dragging {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let p = ui.painter().clone();
        let alpha = if lifted { 0.35 } else { 1.0 };
        let col = theme::text_faint().lerp_to_gamma(theme::text(), h.max(s)).gamma_multiply(alpha);
        match self.style {
            StripStyle::Vertical => {
                if h > 0.0 || s > 0.0 {
                    p.rect_filled(r, CornerRadius::same(8), theme::surface_2().gamma_multiply((0.7 * h).max(0.9 * s) * alpha));
                }
                if s > 0.0 {
                    let x = if self.marker_right { strip.right() - 1.5 } else { strip.left() + 1.5 };
                    p.rect_filled(Rect::from_center_size(Pos2::new(x, r.center().y), Vec2::new(3.0, 20.0 * s)), CornerRadius::same(2), theme::accent());
                }
                paint_view_icon(&p, Rect::from_center_size(r.center(), Vec2::splat(GLYPH)), v, col);
            }
            StripStyle::Row => {
                if h > 0.0 {
                    p.rect_filled(r, CornerRadius::same(6), theme::surface_2().gamma_multiply(0.7 * h * alpha));
                }
                if s > 0.0 {
                    let bar = Rect::from_center_size(Pos2::new(r.center().x, strip.bottom() - 1.0), Vec2::new(16.0 * s, 2.0));
                    p.rect_filled(bar, CornerRadius::same(1), theme::accent());
                }
                paint_view_icon(&p, Rect::from_center_size(r.center(), Vec2::splat(ROW_GLYPH)), v, col);
            }
            StripStyle::Tabs => {
                if h > 0.0 {
                    p.rect_filled(r.shrink2(Vec2::new(0.0, 4.0)), CornerRadius::same(6), theme::surface_2().gamma_multiply(0.6 * h * alpha));
                }
                if s > 0.0 {
                    let bar = Rect::from_min_max(Pos2::new(r.left() + 8.0, r.bottom() - 2.0), Pos2::new(r.left() + 8.0 + (r.width() - 16.0) * s, r.bottom()));
                    p.rect_filled(bar, CornerRadius::same(1), theme::accent());
                }
                let icon = Rect::from_center_size(Pos2::new(r.left() + 12.0 + 7.0, r.center().y), Vec2::splat(14.0));
                paint_view_icon(&p, icon, v, col);
                p.text(Pos2::new(icon.right() + 7.0, r.center().y), Align2::LEFT_CENTER, v.title().to_uppercase(), FontId::proportional(11.0), col);
            }
        }
        if let Some(b) = (self.badge)(v).filter(|b| b.count > 0) {
            let at = match self.style {
                StripStyle::Vertical => Pos2::new(r.right() - 9.0, r.top() + 10.0),
                StripStyle::Row => Pos2::new(r.right() - 6.0, r.top() + 8.0),
                StripStyle::Tabs => Pos2::new(r.right() - 12.0 - badge_width(&p, b) / 2.0, r.center().y),
            };
            paint_badge(&p, at, b, alpha, self.style == StripStyle::Tabs);
        }
        if resp.clicked() {
            events.push(ViewEvent::Click(v));
        }
        resp.context_menu(|ui| self.strip_menu(ui, Some(v), events));
        if !dragging {
            let tip = if selected {
                format!("{} (click to fold)", keybindings::hint(v.title(), view_action(v)))
            } else {
                keybindings::hint(v.title(), view_action(v))
            };
            resp.on_hover_text(tip);
        }
    }

    /// Right-click menu of an icon (`Some(view)`) or the strip's empty space.
    fn strip_menu(&self, ui: &mut Ui, view: Option<View>, events: &mut Vec<ViewEvent>) {
        ui.set_min_width(200.0);
        if let Some(v) = view {
            if let Some(kind) = v.agent() {
                agent_boot_menu(ui, kind, events);
                ui.separator();
            }
            ui.menu_button("Move to", |ui| {
                for c in Container::ALL {
                    if ui.add_enabled(c != self.container, egui::Button::new(c.title())).clicked() {
                        events.push(ViewEvent::Move { view: v, to: c, index: None });
                        ui.close();
                    }
                }
            });
            if ui.button(format!("Hide {}", v.title())).clicked() {
                events.push(ViewEvent::Hide(v));
                ui.close();
            }
            ui.separator();
        }
        ui.label(egui::RichText::new("Views").size(11.0).color(theme::text_faint()));
        for v in View::ALL {
            let mut on = !self.hidden.contains(&v);
            if ui.checkbox(&mut on, v.title()).changed() {
                events.push(if on { ViewEvent::Reveal(v) } else { ViewEvent::Hide(v) });
            }
        }
        if self.container.is_side() {
            ui.separator();
            ui.label(egui::RichText::new("Icons").size(11.0).color(theme::text_faint()));
            for (bar, label) in [(BarOrientation::Side, "On the side"), (BarOrientation::Top, "On top")] {
                if ui.radio(self.state.bar == bar, label).clicked() && self.state.bar != bar {
                    events.push(ViewEvent::SetBar(self.container, bar));
                    ui.close();
                }
            }
        }
        ui.separator();
        if ui.button("Reset layout").clicked() {
            events.push(ViewEvent::Reset);
            ui.close();
        }
    }
}

fn agent_boot_menu(ui: &mut Ui, kind: crate::agents::AgentKind, events: &mut Vec<ViewEvent>) {
    if ui.button(format!("Launch {}", kind.name())).clicked() {
        events.push(ViewEvent::LaunchAgent(kind));
        ui.close();
    }
    if ui.button("Custom Boot…").clicked() {
        events.push(ViewEvent::CustomBoot(kind));
        ui.close();
    }
}

/// A gear at the bottom of the primary strip.
pub fn settings_button(ui: &mut Ui, strip: Rect, events: &mut Vec<ViewEvent>) {
    let r = Rect::from_min_size(Pos2::new(strip.center().x - ITEM / 2.0, strip.bottom() - 8.0 - ITEM), Vec2::splat(ITEM));
    let resp = ui.interact(r, Id::new("view_strip_settings"), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Activity settings"));
    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), theme::anim_time(0.1));
    if h > 0.0 {
        ui.painter().rect_filled(r, CornerRadius::same(8), theme::surface_2().gamma_multiply(0.7 * h));
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    paint_icon(ui.painter(), Rect::from_center_size(r.center(), Vec2::splat(GLYPH - 2.0)), Icon::Gear, theme::text_faint().lerp_to_gamma(theme::text(), h));
    if resp.on_hover_text(keybindings::hint("Settings", Action::OpenSettings)).clicked() {
        events.push(ViewEvent::OpenSettings);
    }
}

fn tab_width(ui: &Ui, v: View, badge: Option<Badge>) -> f32 {
    let text = ui.painter().layout_no_wrap(v.title().to_uppercase(), FontId::proportional(11.0), Color32::WHITE).size().x;
    let extra = badge.filter(|b| b.count > 0).map_or(0.0, |b| badge_width(ui.painter(), b) + 6.0);
    12.0 + 14.0 + 7.0 + text + extra + 12.0
}

fn badge_text(b: Badge) -> String {
    if b.count > 99 { "99+".into() } else { b.count.to_string() }
}

fn badge_width(p: &egui::Painter, b: Badge) -> f32 {
    (p.layout_no_wrap(badge_text(b), FontId::proportional(9.0), Color32::WHITE).size().x + 8.0).max(15.0)
}

fn paint_badge(p: &egui::Painter, c: Pos2, b: Badge, alpha: f32, inline: bool) {
    let (fill, fg) = match b.tone {
        Some(c) => (c, theme::bg()),
        None => (theme::surface_3(), theme::text_dim()),
    };
    let galley = p.layout_no_wrap(badge_text(b), FontId::proportional(9.0), fg.gamma_multiply(alpha));
    let pill = Rect::from_center_size(c, Vec2::new(badge_width(p, b), 15.0));
    // A ring in the strip's colour keeps it apart from the glyph under it.
    let ring = if inline { Stroke::NONE } else { Stroke::new(2.0, theme::bg()) };
    p.rect(pill, CornerRadius::same(8), fill.gamma_multiply(alpha), ring, egui::StrokeKind::Outside);
    p.galley(pill.center() - galley.size() / 2.0, galley, fg);
}

// ---- drag and drop ----------------------------------------------------------------

struct StripTarget {
    container: Container,
    rect: Rect,
    vertical: bool,
    items: Vec<Rect>,
    views: Vec<View>,
}

/// Where a dragged icon can land this frame: strips, container bodies and the window
/// edges around `central` (the terminals).
#[derive(Default)]
pub struct DropTargets {
    strips: Vec<StripTarget>,
    bodies: Vec<(Container, Rect)>,
    pub central: Option<Rect>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Spot {
    Strip { container: Container, index: usize },
    Body(Container),
    Edge(Container, Rect),
}

impl DropTargets {
    pub fn clear(&mut self) {
        self.strips.clear();
        self.bodies.clear();
        self.central = None;
    }

    pub fn add_body(&mut self, c: Container, rect: Rect) {
        self.bodies.push((c, rect));
    }

    /// Paints drop zones and the dragged icon while a drag is on; the move on release.
    /// Call once per frame after every container and the central area are laid out.
    pub fn finish(&self, ctx: &egui::Context, l: &LayoutSettings) -> Option<ViewEvent> {
        let ViewDrag(view) = *egui::DragAndDrop::payload::<ViewDrag>(ctx)?;
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            egui::DragAndDrop::clear_payload(ctx);
            return None;
        }
        let pos = ctx.pointer_latest_pos()?;
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        let p = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("view_drop")));
        let edges = self.edges(l);
        let spot = self.spot_at(pos, &edges);
        for (c, zone, preview) in &edges {
            let hot = matches!(spot, Some(Spot::Edge(t, _)) if t == *c);
            let a = ctx.animate_bool_with_time(Id::new(("view_drop_edge", *c)), hot, theme::anim_time(0.14));
            p.rect(
                *zone,
                CornerRadius::same(8),
                theme::accent().gamma_multiply(0.05),
                Stroke::new(1.0, theme::accent().gamma_multiply(0.22)),
                egui::StrokeKind::Inside,
            );
            if a > 0.0 {
                p.rect(
                    *preview,
                    CornerRadius::same(8),
                    theme::accent().gamma_multiply(0.16 * a),
                    Stroke::new(1.5, theme::accent().gamma_multiply(0.85 * a)),
                    egui::StrokeKind::Inside,
                );
                label(&p, preview.center(), c.title(), a);
            }
        }
        match spot {
            Some(Spot::Strip { container, index }) => self.paint_insertion(&p, container, index, view),
            Some(Spot::Body(c)) => {
                if let Some((_, r)) = self.bodies.iter().find(|(b, _)| *b == c) {
                    p.rect(
                        *r,
                        CornerRadius::same(8),
                        theme::accent().gamma_multiply(0.10),
                        Stroke::new(1.5, theme::accent().gamma_multiply(0.7)),
                        egui::StrokeKind::Inside,
                    );
                    label(&p, r.center(), &format!("Move to {}", c.title().to_lowercase()), 1.0);
                }
            }
            _ => {}
        }
        ghost(&p, pos, view);
        if !ctx.input(|i| i.pointer.any_released()) {
            return None;
        }
        egui::DragAndDrop::clear_payload(ctx);
        Some(match spot? {
            Spot::Strip { container, index } => ViewEvent::Move { view, to: container, index: Some(index) },
            Spot::Body(c) | Spot::Edge(c, _) => ViewEvent::Move { view, to: c, index: None },
        })
    }

    fn spot_at(&self, pos: Pos2, edges: &[(Container, Rect, Rect)]) -> Option<Spot> {
        if let Some(s) = self.strips.iter().find(|s| s.rect.expand(4.0).contains(pos)) {
            let centres: Vec<f32> = s.items.iter().map(|r| if s.vertical { r.center().y } else { r.center().x }).collect();
            let at = if s.vertical { pos.y } else { pos.x };
            return Some(Spot::Strip { container: s.container, index: crate::layout::insertion_index(&centres, at) });
        }
        if let Some((c, _)) = self.bodies.iter().find(|(_, r)| r.contains(pos)) {
            return Some(Spot::Body(*c));
        }
        edges.iter().find(|(_, zone, _)| zone.contains(pos)).map(|(c, _, preview)| Spot::Edge(*c, *preview))
    }

    /// (container, hot zone, preview of where it would appear) for the left, right and
    /// bottom edges of the terminal area.
    fn edges(&self, l: &LayoutSettings) -> Vec<(Container, Rect, Rect)> {
        let Some(c) = self.central else { return Vec::new() };
        let zw = (c.width() * 0.2).clamp(72.0, 160.0);
        let zh = (c.height() * 0.25).clamp(64.0, 140.0);
        let side_w = |k: Container| l.views.size(k).min(c.width() * 0.45);
        let left = l.container_at(Side::Left);
        let right = l.container_at(Side::Right);
        let pad = 6.0;
        let left_zone = Rect::from_min_max(c.min, Pos2::new(c.left() + zw, c.bottom())).shrink(pad);
        let right_zone = Rect::from_min_max(Pos2::new(c.right() - zw, c.top()), c.max).shrink(pad);
        let bottom_zone = Rect::from_min_max(Pos2::new(c.left() + zw, c.bottom() - zh), Pos2::new(c.right() - zw, c.bottom())).shrink(pad);
        let left_preview = Rect::from_min_max(c.min, Pos2::new(c.left() + side_w(left), c.bottom())).shrink(pad);
        let right_preview = Rect::from_min_max(Pos2::new(c.right() - side_w(right), c.top()), c.max).shrink(pad);
        let panel_h = l.views.size(Container::Panel).min(c.height() * 0.5);
        let bottom_preview = Rect::from_min_max(Pos2::new(c.left(), c.bottom() - panel_h), c.max).shrink(pad);
        vec![(left, left_zone, left_preview), (right, right_zone, right_preview), (Container::Panel, bottom_zone, bottom_preview)]
    }

    fn paint_insertion(&self, p: &egui::Painter, container: Container, index: usize, view: View) {
        let Some(s) = self.strips.iter().find(|s| s.container == container) else { return };
        // Dropping right before or after itself changes nothing.
        if let Some(cur) = s.views.iter().position(|v| *v == view)
            && (index == cur || index == cur + 1)
        {
            return;
        }
        let stroke = Stroke::new(2.0, theme::accent());
        if s.vertical {
            let y = match (s.items.get(index), s.items.last()) {
                (Some(r), _) => r.top() - GAP / 2.0,
                (None, Some(last)) => last.bottom() + GAP / 2.0,
                (None, None) => s.rect.top() + 8.0,
            };
            let x = s.rect.x_range().shrink(8.0);
            p.hline(x, y, stroke);
            p.circle_filled(Pos2::new(x.min, y), 3.0, theme::accent());
        } else {
            let x = match (s.items.get(index), s.items.last()) {
                (Some(r), _) => r.left() - GAP / 2.0,
                (None, Some(last)) => last.right() + GAP / 2.0,
                (None, None) => s.rect.left() + 8.0,
            };
            let y = s.rect.y_range().shrink(6.0);
            p.vline(x, y, stroke);
            p.circle_filled(Pos2::new(x, y.min), 3.0, theme::accent());
        }
    }
}

fn label(p: &egui::Painter, c: Pos2, text: &str, alpha: f32) {
    let galley = p.layout_no_wrap(text.to_string(), FontId::proportional(12.0), theme::text().gamma_multiply(alpha));
    let pill = Rect::from_center_size(c, galley.size() + Vec2::new(20.0, 10.0));
    p.rect(
        pill,
        CornerRadius::same(8),
        theme::surface_2().gamma_multiply(0.95 * alpha),
        Stroke::new(1.0, theme::border().gamma_multiply(alpha)),
        egui::StrokeKind::Inside,
    );
    p.galley(pill.center() - galley.size() / 2.0, galley, theme::text());
}

/// The icon (and its name) following the pointer.
fn ghost(p: &egui::Painter, pos: Pos2, v: View) {
    let tile = Rect::from_min_size(pos + Vec2::new(10.0, 10.0), Vec2::splat(32.0));
    let galley = p.layout_no_wrap(v.title().to_string(), FontId::proportional(12.0), theme::text());
    let card = Rect::from_min_size(tile.min, Vec2::new(tile.width() + galley.size().x + 14.0, tile.height()));
    p.rect_filled(card.translate(Vec2::new(0.0, 3.0)), CornerRadius::same(8), theme::shadow().gamma_multiply(0.35));
    p.rect(card, CornerRadius::same(8), theme::surface_3(), Stroke::new(1.0, theme::accent().gamma_multiply(0.7)), egui::StrokeKind::Inside);
    paint_view_icon(p, Rect::from_center_size(tile.center(), Vec2::splat(16.0)), v, theme::text());
    p.galley(Pos2::new(tile.right(), card.center().y - galley.size().y / 2.0), galley, theme::text());
}
