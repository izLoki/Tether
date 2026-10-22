//! The agent status pill (like Warp's): a coloured mark and a short label next to a
//! tab's title, in vertical tab rows and over a terminal pane. Clicking it opens a
//! small popover with the session's details and quick actions.

use std::sync::Mutex;
use std::time::Instant;

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Id, Painter, Pos2, Rect, RichText, Sense, Shape, Stroke, StrokeKind, Vec2};

use super::{agent_badge, agent_icons, agents_panel};
use crate::agents::{AgentKind, AgentState, SessionInfo, snippet};
use crate::procmon::human_mem;
use crate::theme;

pub const PILL_H: f32 = 18.0;
pub const FONT_SIZE: f32 = 10.5;
/// Left padding, mark and gap before the label.
pub const LEAD: f32 = 6.0 + 8.0 + 4.0;
/// Right padding after the label.
pub const TAIL: f32 = 7.0;
/// Width of the dot-only pill in compact layouts.
pub const COMPACT_W: f32 = 14.0;
const POPOVER_W: f32 = 300.0;
const MESSAGE_CHARS: usize = 280;

/// Semantic colour of a state, resolved against the theme by [`tone_color`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Accent,
    Warn,
    Success,
    Danger,
    Faint,
}

/// The shape painted at the left of the pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Dot,
    Ring,
    Warning,
    Check,
    Cross,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PillSpec {
    /// `None`: idle sessions show only a faint ring.
    pub label: Option<&'static str>,
    pub tone: Tone,
    pub mark: Mark,
    /// Breathes while visible (and animations are on).
    pub pulse: bool,
}

pub fn spec(state: AgentState) -> PillSpec {
    let s = |label, tone, mark| PillSpec { label: Some(label), tone, mark, pulse: false };
    match state {
        AgentState::Idle => PillSpec { label: None, tone: Tone::Faint, mark: Mark::Ring, pulse: false },
        AgentState::Working => PillSpec { pulse: true, ..s("Working", Tone::Accent, Mark::Dot) },
        AgentState::NeedsInput => s("Needs input", Tone::Warn, Mark::Dot),
        AgentState::NeedsPermission => s("Permission", Tone::Warn, Mark::Warning),
        AgentState::Done => s("Done", Tone::Success, Mark::Check),
        AgentState::Error => s("Error", Tone::Danger, Mark::Cross),
    }
}

pub fn tone_color(tone: Tone) -> Color32 {
    match tone {
        Tone::Accent => theme::accent(),
        Tone::Warn => theme::warn(),
        Tone::Success => theme::success(),
        Tone::Danger => theme::danger(),
        Tone::Faint => theme::text_faint(),
    }
}

pub fn font() -> FontId {
    FontId::proportional(FONT_SIZE)
}

/// Width of the pill for `state` (`compact`: just the mark).
pub fn width(p: &Painter, state: AgentState, compact: bool) -> f32 {
    match spec(state).label {
        Some(label) if !compact => LEAD + p.layout_no_wrap(label.to_string(), font(), Color32::WHITE).size().x + TAIL,
        _ => COMPACT_W,
    }
}

/// Paints the pill in `rect`; `hover` (0..1) brightens it.
pub fn paint(p: &Painter, rect: Rect, state: AgentState, time: f64, hover: f32) {
    let sp = spec(state);
    let c = tone_color(sp.tone);
    let has_label = sp.label.is_some() && rect.width() > COMPACT_W + 1.0;
    if has_label {
        let fill = c.gamma_multiply(0.14 + 0.1 * hover);
        p.rect(rect, CornerRadius::same((rect.height() / 2.0) as u8), fill, Stroke::new(1.0, c.gamma_multiply(0.28 + 0.3 * hover)), StrokeKind::Inside);
    } else if hover > 0.0 {
        p.rect_filled(rect, CornerRadius::same((rect.height() / 2.0) as u8), theme::surface_3().gamma_multiply(hover));
    }
    let mark_c = if has_label { Pos2::new(rect.left() + 6.0 + 4.0, rect.center().y) } else { rect.center() };
    let k = if sp.pulse { agent_badge::pulse(time) } else { 1.0 };
    paint_mark(p, mark_c, sp.mark, c, k);
    if let (true, Some(label)) = (has_label, sp.label) {
        let text_c = label_color(sp.tone);
        p.text(Pos2::new(rect.left() + LEAD, rect.center().y), Align2::LEFT_CENTER, label, font(), text_c);
    }
}

/// Label text: the tone, pulled towards the text colour so amber and green stay
/// readable on light themes.
fn label_color(tone: Tone) -> Color32 {
    let c = tone_color(tone);
    if theme::palette().dark { c } else { c.lerp_to_gamma(theme::text(), 0.45) }
}

fn paint_mark(p: &Painter, c: Pos2, mark: Mark, colour: Color32, pulse: f32) {
    let r = 3.5;
    match mark {
        Mark::Dot => {
            if pulse < 1.0 {
                p.circle_filled(c, r * (1.0 + 0.7 * (1.0 - pulse)), colour.gamma_multiply(0.3 * pulse));
            }
            p.circle_filled(c, r, colour.gamma_multiply(0.55 + 0.45 * pulse));
        }
        Mark::Ring => {
            p.circle_stroke(c, r - 0.4, Stroke::new(1.2, colour));
        }
        Mark::Warning => {
            let pts = vec![c + Vec2::new(0.0, -4.4), c + Vec2::new(4.4, 3.6), c + Vec2::new(-4.4, 3.6)];
            p.add(Shape::convex_polygon(pts, colour, Stroke::NONE));
            let ink = theme::bg();
            p.line_segment([c + Vec2::new(0.0, -1.4), c + Vec2::new(0.0, 1.0)], Stroke::new(1.1, ink));
            p.circle_filled(c + Vec2::new(0.0, 2.4), 0.6, ink);
        }
        Mark::Check => {
            p.add(Shape::line(vec![c + Vec2::new(-3.4, 0.2), c + Vec2::new(-1.0, 2.6), c + Vec2::new(3.6, -2.6)], Stroke::new(1.6, colour)));
        }
        Mark::Cross => {
            let s = Stroke::new(1.6, colour);
            p.line_segment([c + Vec2::splat(-2.8), c + Vec2::splat(2.8)], s);
            p.line_segment([c + Vec2::new(2.8, -2.8), c + Vec2::new(-2.8, 2.8)], s);
        }
    }
}

/// `Claude Code · Working`
pub fn tooltip(kind: AgentKind, state: AgentState) -> String {
    format!("{} · {}\nClick for details", kind.name(), state.label())
}

/// The pill as a button at `rect`; returns its response (pointer cursor and tooltip on hover).
pub fn button(ui: &mut egui::Ui, rect: Rect, id: Id, kind: AgentKind, state: AgentState) -> egui::Response {
    let resp = ui.interact(rect, id, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Agent status {}", spec(state).label.unwrap_or("Idle"))));
    let hover = ui.ctx().animate_bool_with_time(id.with("hover"), resp.hovered(), theme::anim_time(0.1));
    let time = ui.input(|i| i.time);
    paint(ui.painter(), rect, state, time, hover);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text(tooltip(kind, state))
}

// ---- popover ------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Open {
    uid: u64,
    anchor: Rect,
    /// The pass it was opened in (the opening click is not a click outside).
    pass: u64,
}

static OPEN: Mutex<Option<Open>> = Mutex::new(None);

fn open_state() -> Option<Open> {
    OPEN.lock().ok().and_then(|o| *o)
}

fn set_open(value: Option<Open>) {
    if let Ok(mut o) = OPEN.lock() {
        *o = value;
    }
}

/// Opens the popover of tab `uid` under `anchor`, or closes it if it is already open.
pub fn toggle_popover(ctx: &egui::Context, uid: u64, anchor: Rect) {
    match open_state() {
        Some(o) if o.uid == uid => set_open(None),
        _ => set_open(Some(Open { uid, anchor, pass: ctx.cumulative_pass_nr() })),
    }
}

/// The tab whose popover is open.
pub fn popover_uid() -> Option<u64> {
    open_state().map(|o| o.uid)
}

pub fn close_popover() {
    set_open(None);
}

pub enum PopoverAction {
    Jump(u64),
    MarkRead(u64),
    InstallHooks,
}

/// What the popover shows about one session.
pub struct PopoverData<'a> {
    pub info: &'a SessionInfo,
    /// The tab's title (the session's name).
    pub title: &'a str,
    /// The agent's hooks are installed in its config (exact states).
    pub hooks_installed: bool,
    pub now: Instant,
}

/// Text a program printed, made safe to show: one line, no control characters.
pub fn sanitize(text: &str, max: usize) -> String {
    let clean: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    snippet(&clean, max)
}

/// Shows the open popover (if any) for `data`; `None` data closes it.
pub fn show_popover(ctx: &egui::Context, data: Option<PopoverData<'_>>) -> Option<PopoverAction> {
    let open = open_state()?;
    let Some(data) = data.filter(|d| d.info.uid == open.uid) else {
        close_popover();
        return None;
    };
    let mut action = None;
    let screen = ctx.content_rect();
    let x = open.anchor.left().min(screen.right() - POPOVER_W - 12.0).max(screen.left() + 8.0);
    let area = egui::Area::new(Id::new("agent_status_popover"))
        .order(egui::Order::Foreground)
        .fixed_pos(Pos2::new(x, open.anchor.bottom() + 6.0))
        .constrain(true)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(12)).show(ui, |ui| {
                ui.set_width(POPOVER_W - 24.0);
                action = popover_body(ui, &data);
            })
        });
    let outside = ctx.input(|i| i.pointer.any_click() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p)));
    let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    if action.is_some() || esc || (outside && ctx.cumulative_pass_nr() != open.pass) {
        close_popover();
    }
    action
}

fn popover_body(ui: &mut egui::Ui, d: &PopoverData<'_>) -> Option<PopoverAction> {
    let s = d.info;
    let mut action = None;
    ui.spacing_mut().item_spacing.y = 6.0;
    ui.horizontal(|ui| {
        agent_icons::icon(ui, s.kind, 16.0);
        ui.label(RichText::new(s.kind.name()).size(12.5).strong().color(theme::text()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let w = width(ui.painter(), s.state, false);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(w, PILL_H), Sense::hover());
            paint(ui.painter(), rect, s.state, ui.input(|i| i.time), 0.0);
        });
    });
    ui.label(RichText::new(d.title).size(13.0).color(theme::text()));
    let age = agents_panel::elapsed(d.now.saturating_duration_since(s.since));
    ui.label(RichText::new(format!("{} for {age}", s.state.label())).size(11.0).color(theme::text_dim()));
    if let Some(m) = &s.message {
        let heading = match (s.state, s.message_from_terminal) {
            (_, true) => "From terminal output",
            (AgentState::NeedsPermission, _) => "Permission request",
            _ => "Last message",
        };
        egui::Frame::new().fill(theme::surface_2()).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(heading).size(10.0).color(theme::text_faint()));
            ui.add(egui::Label::new(RichText::new(sanitize(m, MESSAGE_CHARS)).size(11.5).color(theme::text())).wrap());
        });
    }
    let dim = |ui: &mut egui::Ui, text: String| ui.label(RichText::new(text).size(11.0).color(theme::text_faint()));
    if let Some(cwd) = &s.cwd {
        dim(ui, super::tabs::short_path(cwd));
    }
    if let Some(u) = &s.usage {
        dim(ui, format!("CPU {:.0}%  ·  {}  ·  {} process{}", u.cpu, human_mem(u.mem), u.procs, if u.procs == 1 { "" } else { "es" }));
    }
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let jump = egui::Button::new(RichText::new("Jump to tab").size(12.0).color(theme::on_accent()))
            .fill(theme::accent())
            .corner_radius(CornerRadius::same(6))
            .min_size(Vec2::new(0.0, 26.0));
        if ui.add(jump).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            action = Some(PopoverAction::Jump(s.uid));
        }
        let unread = s.state.needs_attention();
        let read = egui::Button::new(RichText::new("Mark as read").size(12.0))
            .fill(theme::surface_3())
            .corner_radius(CornerRadius::same(6))
            .min_size(Vec2::new(0.0, 26.0));
        let resp = ui.add_enabled(unread, read).on_disabled_hover_text("Nothing new in this session");
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            action = Some(PopoverAction::MarkRead(s.uid));
        }
    });
    if !s.hooked && !d.hooks_installed && matches!(s.kind, AgentKind::Claude | AgentKind::Codex) {
        let label = if s.kind == AgentKind::Codex { "Set up completion notifications…" } else { "Install hooks for exact status…" };
        let link = ui.add(egui::Button::new(RichText::new(label).size(11.5).color(theme::accent())).frame(false));
        if link.on_hover_text("Configure the agent signals Tether uses to follow this session").on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            action = Some(PopoverAction::InstallHooks);
        }
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_map_to_labels_and_tones() {
        let cases = [
            (AgentState::Working, Some("Working"), Tone::Accent, Mark::Dot),
            (AgentState::NeedsInput, Some("Needs input"), Tone::Warn, Mark::Dot),
            (AgentState::NeedsPermission, Some("Permission"), Tone::Warn, Mark::Warning),
            (AgentState::Done, Some("Done"), Tone::Success, Mark::Check),
            (AgentState::Error, Some("Error"), Tone::Danger, Mark::Cross),
            (AgentState::Idle, None, Tone::Faint, Mark::Ring),
        ];
        for (state, label, tone, mark) in cases {
            let s = spec(state);
            assert_eq!((s.label, s.tone, s.mark), (label, tone, mark), "{state:?}");
            assert_eq!(s.pulse, state == AgentState::Working);
        }
    }

    #[test]
    fn tones_resolve_to_theme_colours() {
        assert_eq!(tone_color(Tone::Accent), theme::accent());
        assert_eq!(tone_color(Tone::Warn), theme::warn());
        assert_eq!(tone_color(Tone::Success), theme::success());
        assert_eq!(tone_color(Tone::Danger), theme::danger());
    }

    #[test]
    fn printed_text_is_sanitized() {
        assert_eq!(sanitize("Allow\x1b[31m rm -rf\x07\n now?", 100), "Allow [31m rm -rf now?");
        assert_eq!(sanitize("abcdefgh", 4), "abc…");
    }

    #[test]
    fn popover_toggles_per_tab() {
        let ctx = egui::Context::default();
        let r = Rect::from_min_size(Pos2::ZERO, Vec2::splat(10.0));
        close_popover();
        toggle_popover(&ctx, 7, r);
        assert_eq!(popover_uid(), Some(7));
        toggle_popover(&ctx, 8, r);
        assert_eq!(popover_uid(), Some(8), "another tab's pill moves it");
        toggle_popover(&ctx, 8, r);
        assert_eq!(popover_uid(), None, "the same pill closes it");
    }
}
