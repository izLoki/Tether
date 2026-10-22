//! Form building blocks shared by the host editor and the key manager: section cards
//! with a title and a one-line description, stacked labels, tall framed inputs with a
//! focus ring, a select box, a password field with a show / hide eye, full-width
//! segmented control and 36 px buttons. Everything is painted from `theme` tokens so
//! light and dark themes look deliberate.

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Margin, Pos2, Rect, Response, RichText, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, layers::ShapeIdx,
};

use super::settings_widgets::{self as w, Icon};
use crate::theme;

pub const INPUT_H: f32 = 36.0;
pub const GAP: f32 = 12.0;
const RADIUS: u8 = 8;

pub fn modal_frame() -> egui::Frame {
    egui::Frame::new().fill(theme::surface()).stroke(Stroke::new(1.0, theme::border())).corner_radius(16).shadow(egui::Shadow {
        offset: [0, 18],
        blur: 48,
        spread: 0,
        color: theme::shadow(),
    })
}

fn hand(ui: &Ui, resp: &Response) {
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
}

/// Fill of an input sitting on a section card: inset in dark themes, paper in light.
pub fn input_fill() -> Color32 {
    if theme::palette().dark { theme::bg() } else { theme::surface() }
}

/// Card fill for sections inside a dialog painted with `theme::surface()`.
pub fn card_fill() -> Color32 {
    theme::surface_2()
}

/// Section card: icon tile, title, one-line description, then the body.
pub fn section<R>(ui: &mut Ui, icon: Option<Icon>, title: &str, desc: &str, body: impl FnOnce(&mut Ui) -> R) -> R {
    section_with(ui, icon, title, desc, |_| {}, body).1
}

/// [`section`] with controls right-aligned in the header (e.g. "Manage keys");
/// returns what `trailing` and `body` returned.
pub fn section_with<T, R>(
    ui: &mut Ui,
    icon: Option<Icon>,
    title: &str,
    desc: &str,
    trailing: impl FnOnce(&mut Ui) -> T,
    body: impl FnOnce(&mut Ui) -> R,
) -> (T, R) {
    egui::Frame::new()
        .fill(card_fill())
        .stroke(Stroke::new(1.0, theme::border().gamma_multiply(0.8)))
        .corner_radius(12)
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let t = section_header(ui, icon, title, desc, trailing);
            ui.add_space(14.0);
            (t, body(ui))
        })
        .inner
}

pub fn section_header<T>(ui: &mut Ui, icon: Option<Icon>, title: &str, desc: &str, trailing: impl FnOnce(&mut Ui) -> T) -> T {
    ui.horizontal(|ui| {
        if let Some(icon) = icon {
            icon_tile(ui, icon, theme::accent());
            ui.add_space(4.0);
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(title).size(14.0).strong().color(theme::text()));
            if !desc.is_empty() {
                ui.label(RichText::new(desc).size(12.0).color(theme::text_dim()));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(Align::Center), trailing).inner
    })
    .inner
}

/// 32 px rounded tile with a line icon, tinted `col`.
pub fn icon_tile(ui: &mut Ui, icon: Icon, col: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(9), col.gamma_multiply(0.14));
    w::paint_icon(ui.painter(), rect.shrink(8.5), icon, col);
}

/// Small label above a field. `error` turns it red.
pub fn label(ui: &mut Ui, text: &str, error: bool) {
    let col = if error { theme::danger() } else { theme::text_dim() };
    ui.label(RichText::new(text).size(12.0).strong().color(col));
    ui.add_space(-1.0);
}

/// One line of help (or an error) under a field.
pub fn note(ui: &mut Ui, text: &str, error: bool) {
    let col = if error { theme::danger() } else { theme::text_faint() };
    ui.add(egui::Label::new(RichText::new(text).size(11.5).color(col)).wrap());
}

/// Label + control + optional error / help line, stacked, at a fixed width.
pub fn field<R>(ui: &mut Ui, width: f32, text: &str, error: Option<&str>, help: Option<&str>, control: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(Vec2::new(width, 0.0), egui::Layout::top_down(Align::Min), |ui| {
        ui.set_width(width);
        ui.spacing_mut().item_spacing.y = 6.0;
        label(ui, text, error.is_some());
        let out = control(ui);
        match (error, help) {
            (Some(e), _) => note(ui, e, true),
            (None, Some(h)) => note(ui, h, false),
            _ => {}
        }
        out
    })
    .inner
}

/// Two fields side by side: `add(ui, left_w, right_w)` with `right_w` fixed and the
/// left column taking the rest. Lay out the left field first.
pub fn row2(ui: &mut Ui, right_w: f32, add: impl FnOnce(&mut Ui, f32, f32)) {
    let left_w = (ui.available_width() - right_w - GAP).max(80.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        add(ui, left_w, right_w);
    });
}

/// Paints an input frame under whatever was drawn after `bg` was reserved.
pub fn paint_frame(ui: &Ui, bg: ShapeIdx, id: egui::Id, rect: Rect, focused: bool, hovered: bool, error: bool) {
    let ctx = ui.ctx();
    let id = id.with("frame");
    let f = ctx.animate_bool_with_time(id.with("f"), focused, theme::anim_time(0.12));
    let h = ctx.animate_bool_with_time(id.with("h"), hovered, theme::anim_time(0.1));
    let rest = theme::border().lerp_to_gamma(theme::text_faint(), 0.45 * h);
    let hot = if error { theme::danger() } else { theme::accent() };
    let stroke_col = if error { theme::danger().gamma_multiply(0.85 + 0.15 * f) } else { rest.lerp_to_gamma(hot, f) };
    let mut shapes = Vec::with_capacity(2);
    if f > 0.0 {
        shapes.push(Shape::rect_filled(rect.expand(3.0), CornerRadius::same(RADIUS + 3), hot.gamma_multiply(0.18 * f)));
    }
    shapes.push(Shape::Rect(egui::epaint::RectShape::new(
        rect,
        CornerRadius::same(RADIUS),
        input_fill(),
        Stroke::new(1.0 + 0.5 * f, stroke_col),
        StrokeKind::Inside,
    )));
    ui.painter().set(bg, Shape::Vec(shapes));
}

fn edit(edit: egui::TextEdit<'_>, width: f32, trailing: f32) -> egui::TextEdit<'_> {
    edit.frame(egui::Frame::new().inner_margin(Margin { left: 12, right: 12 + trailing as i8, top: 9, bottom: 9 }))
        .min_size(Vec2::new(width, INPUT_H))
        .desired_width(width - 24.0 - trailing)
        .vertical_align(Align::Center)
        .font(FontId::proportional(13.5))
}

/// Tall single-line text input.
pub fn text(ui: &mut Ui, value: &mut String, hint: &str, width: f32, error: bool) -> Response {
    framed(ui, error, |ui| ui.add(edit(egui::TextEdit::singleline(value).hint_text(hint), width, 0.0)))
}

/// Wraps a custom-built `TextEdit` (e.g. one with autocomplete) in the input frame.
/// Build the edit with [`style_edit`].
pub fn framed(ui: &mut Ui, error: bool, add: impl FnOnce(&mut Ui) -> Response) -> Response {
    let bg = ui.painter().add(Shape::Noop);
    let resp = add(ui);
    paint_frame(ui, bg, resp.id, resp.rect, resp.has_focus(), resp.hovered(), error);
    resp
}

/// Applies the input look to an edit built elsewhere.
pub fn style_edit(e: egui::TextEdit<'_>, width: f32) -> egui::TextEdit<'_> {
    edit(e, width, 0.0)
}

/// Password / passphrase input with an eye button inside its right edge.
pub fn secret(ui: &mut Ui, value: &mut String, hint: &str, shown: &mut bool, width: f32, error: bool) -> Response {
    let bg = ui.painter().add(Shape::Noop);
    let resp = ui.add(edit(egui::TextEdit::singleline(value).password(!*shown).hint_text(hint), width, 26.0));
    let eye_rect = Rect::from_center_size(Pos2::new(resp.rect.right() - 20.0, resp.rect.center().y), Vec2::splat(26.0));
    let eye = ui.interact(eye_rect, resp.id.with("eye"), Sense::click()).on_hover_text(if *shown { "Hide" } else { "Show" });
    hand(ui, &eye);
    if eye.clicked() {
        *shown = !*shown;
    }
    let h = ui.ctx().animate_bool_with_time(eye.id, eye.hovered(), theme::anim_time(0.1));
    if h > 0.0 {
        ui.painter().rect_filled(eye_rect, CornerRadius::same(6), theme::surface_3().gamma_multiply(h));
    }
    paint_eye(ui.painter(), eye_rect.center(), !*shown, theme::text_dim().lerp_to_gamma(theme::text(), h));
    paint_frame(ui, bg, resp.id, resp.rect, resp.has_focus(), resp.hovered() || eye.hovered(), error);
    resp
}

/// Eye glyph; `slashed` when the text is hidden.
pub fn paint_eye(p: &egui::Painter, c: Pos2, slashed: bool, col: Color32) {
    let s = Stroke::new(1.4, col);
    let (rx, ry) = (7.0, 4.6);
    let top: Vec<Pos2> = (0..=12)
        .map(|i| {
            let t = std::f32::consts::PI * i as f32 / 12.0;
            c + Vec2::new(-rx * t.cos(), -ry * t.sin())
        })
        .collect();
    let bottom: Vec<Pos2> = top.iter().map(|q| Pos2::new(q.x, 2.0 * c.y - q.y)).collect();
    p.add(Shape::line(top, s));
    p.add(Shape::line(bottom, s));
    p.circle_stroke(c, 2.1, s);
    if slashed {
        p.line_segment([c + Vec2::new(-6.5, -6.0), c + Vec2::new(6.5, 6.0)], Stroke::new(1.4, col));
    }
}

/// A select box styled like an input; `body` fills the dropdown.
pub fn select(ui: &mut Ui, id: egui::Id, width: f32, shown: &str, body: impl FnOnce(&mut Ui)) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, INPUT_H), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let open = egui::Popup::is_id_open(ui.ctx(), id);
    let bg = ui.painter().add(Shape::Noop);
    let clip = ui.painter().with_clip_rect(rect.shrink2(Vec2::new(10.0, 0.0)));
    clip.text(Pos2::new(rect.left() + 12.0, rect.center().y), Align2::LEFT_CENTER, shown, FontId::proportional(13.5), theme::text());
    let chev = Pos2::new(rect.right() - 18.0, rect.center().y);
    let t = ui.ctx().animate_bool_with_time(id.with("chev"), open, theme::anim_time(0.14));
    paint_chevron(ui.painter(), chev, t, theme::text_dim());
    paint_frame(ui, bg, resp.id, rect, open, resp.hovered(), false);
    egui::Popup::menu(&resp).id(id).width(width).gap(6.0).show(|ui| {
        ui.set_min_width(width - 12.0);
        body(ui);
    });
    resp
}

/// Down chevron that rotates to point up as `t` goes 0 → 1.
pub fn paint_chevron(p: &egui::Painter, c: Pos2, t: f32, col: Color32) {
    let dir = 1.0 - 2.0 * t;
    let pts = vec![c + Vec2::new(-4.5, -2.2 * dir), c + Vec2::new(0.0, 2.3 * dir), c + Vec2::new(4.5, -2.2 * dir)];
    p.add(Shape::line(pts, Stroke::new(1.5, col)));
}

/// Row in a dropdown: text plus a check when selected.
pub fn option(ui: &mut Ui, label: &str, sub: Option<&str>, selected: bool) -> Response {
    let h = if sub.is_some() { 42.0 } else { 32.0 };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::click());
    hand(ui, &resp);
    let hov = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), theme::anim_time(0.08));
    let p = ui.painter();
    if hov > 0.0 || selected {
        let fill = if selected { theme::accent_soft() } else { theme::surface_3().gamma_multiply(hov) };
        p.rect_filled(rect, CornerRadius::same(6), fill);
    }
    let x = rect.left() + 10.0;
    match sub {
        Some(s) => {
            p.text(Pos2::new(x, rect.top() + 13.0), Align2::LEFT_CENTER, label, FontId::proportional(13.0), theme::text());
            p.text(Pos2::new(x, rect.top() + 30.0), Align2::LEFT_CENTER, s, FontId::proportional(11.0), theme::text_faint());
        }
        None => {
            p.text(Pos2::new(x, rect.center().y), Align2::LEFT_CENTER, label, FontId::proportional(13.0), theme::text());
        }
    }
    if selected {
        w::paint_icon(p, Rect::from_center_size(Pos2::new(rect.right() - 16.0, rect.center().y), Vec2::splat(11.0)), Icon::Check, theme::accent());
    }
    resp
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Primary,
    Secondary,
    Danger,
    Ghost,
}

/// 36 px button. `enabled = false` greys it out and ignores clicks.
pub fn button(ui: &mut Ui, label: &str, kind: Kind, enabled: bool) -> Response {
    let font = FontId::proportional(13.0);
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, theme::text());
    let size = Vec2::new((galley.size().x + 32.0).max(84.0), INPUT_H);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    if enabled {
        hand(ui, &resp);
    }
    let h = ui.ctx().animate_bool_with_time(resp.id, enabled && resp.hovered(), theme::anim_time(0.1));
    let pressed = resp.is_pointer_button_down_on();
    let (fill, stroke, text) = match kind {
        Kind::Primary => (theme::accent().lerp_to_gamma(Color32::WHITE, 0.08 * h), Stroke::NONE, theme::on_accent()),
        Kind::Danger => (theme::danger().gamma_multiply(0.85 + 0.15 * h), Stroke::NONE, Color32::WHITE),
        Kind::Secondary => (
            theme::surface_3().lerp_to_gamma(theme::border(), 0.5 * h),
            Stroke::new(1.0, theme::border().lerp_to_gamma(theme::text_faint(), 0.4 * h)),
            theme::text(),
        ),
        Kind::Ghost => (theme::surface_3().gamma_multiply(h), Stroke::NONE, theme::text_dim().lerp_to_gamma(theme::text(), h)),
    };
    let rect = if pressed { rect.shrink(0.5) } else { rect };
    let alpha = if enabled { 1.0 } else { 0.45 };
    let p = ui.painter();
    if kind == Kind::Primary && enabled {
        p.rect_filled(rect.translate(Vec2::new(0.0, 1.0)), CornerRadius::same(RADIUS), theme::shadow().gamma_multiply(0.35));
    }
    p.rect(rect, CornerRadius::same(RADIUS), fill.gamma_multiply(alpha), stroke, StrokeKind::Inside);
    p.galley_with_override_text_color(rect.center() - galley.size() / 2.0, galley, text.gamma_multiply(alpha));
    resp
}

/// Full-width segmented control with a sliding pill; each option has a title and a hint.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str, &str)]) -> Response {
    let width = ui.available_width();
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(width, 52.0), Sense::hover());
    let n = options.len().max(1) as f32;
    let seg_w = (rect.width() - 8.0) / n;
    let selected = options.iter().position(|(v, ..)| v == value).unwrap_or(0);
    let pos = ui.ctx().animate_value_with_time(resp.id.with("pill"), selected as f32, theme::anim_time(0.2));
    let p = ui.painter().clone();
    p.rect(rect, CornerRadius::same(11), input_fill(), Stroke::new(1.0, theme::border()), StrokeKind::Inside);
    let seg = |i: f32| Rect::from_min_size(Pos2::new(rect.left() + 4.0 + i * seg_w, rect.top() + 4.0), Vec2::new(seg_w, rect.height() - 8.0));
    let pill = seg(pos);
    p.rect_filled(pill.translate(Vec2::new(0.0, 1.0)), CornerRadius::same(8), theme::shadow().gamma_multiply(0.3));
    p.rect(pill, CornerRadius::same(8), theme::surface_3(), Stroke::new(1.0, theme::accent().gamma_multiply(0.55)), StrokeKind::Inside);
    for (i, (v, title, hint)) in options.iter().enumerate() {
        let r = seg(i as f32);
        let item = ui.interact(r, resp.id.with(i), Sense::click());
        item.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, i == selected, *title));
        hand(ui, &item);
        if item.clicked() && *v != *value {
            *value = *v;
            resp.mark_changed();
        }
        let active = i == selected;
        let col = if active || item.hovered() { theme::text() } else { theme::text_dim() };
        p.text(Pos2::new(r.center().x, r.top() + 15.0), Align2::CENTER_CENTER, *title, FontId::proportional(13.0), col);
        let hint_col = if active { theme::text_dim() } else { theme::text_faint() };
        p.text(Pos2::new(r.center().x, r.top() + 31.0), Align2::CENTER_CENTER, *hint, FontId::proportional(10.5), hint_col);
    }
    resp
}

/// Thin rule across the available width.
pub fn rule(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0, theme::border().gamma_multiply(0.7)));
}

/// Dashed rounded outline (e.g. a "create new" card).
pub fn dashed(p: &egui::Painter, rect: Rect, col: Color32) {
    let s = Stroke::new(1.2, col);
    let r = rect.shrink(0.5);
    for (a, b) in [(r.left_top(), r.right_top()), (r.right_top(), r.right_bottom()), (r.right_bottom(), r.left_bottom()), (r.left_bottom(), r.left_top())] {
        p.extend(Shape::dashed_line(&[a, b], s, 5.0, 4.0));
    }
}

/// Small pill tag.
pub fn tag(ui: &mut Ui, text: &str, col: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(11.0), col);
    let (rect, _) = ui.allocate_exact_size(galley.size() + Vec2::new(14.0, 6.0), Sense::hover());
    paint_tag(ui.painter(), rect, galley, col);
}

pub fn paint_tag(p: &egui::Painter, rect: Rect, galley: std::sync::Arc<egui::Galley>, col: Color32) {
    p.rect_filled(rect, CornerRadius::same(7), col.gamma_multiply(0.15));
    p.galley(rect.min + Vec2::new(7.0, 3.0), galley, col);
}

/// Keyboard hint like `Enter save · Esc cancel`, drawn as faint text with key chips.
pub fn hints(ui: &mut Ui, pairs: &[(&str, &str)]) {
    ui.spacing_mut().item_spacing.x = 5.0;
    // In a right-to-left layout the last pair is placed first and each pair reads chip-first.
    let rtl = ui.layout().prefer_right_to_left();
    let mut items: Vec<&(&str, &str)> = pairs.iter().collect();
    if rtl {
        items.reverse();
    }
    for (i, (key, what)) in items.into_iter().enumerate() {
        if i > 0 {
            ui.add_space(6.0);
        }
        let what = RichText::new(*what).size(11.5).color(theme::text_faint());
        if rtl {
            ui.label(what);
            w::key_chip(ui, key);
        } else {
            w::key_chip(ui, key);
            ui.label(what);
        }
    }
}

/// Status banner: a tinted rounded strip with a check (ok) or an info mark (error).
pub fn banner(ui: &mut Ui, ok: bool, text: &str) {
    let col = if ok { theme::accent() } else { theme::danger() };
    egui::Frame::new()
        .fill(col.gamma_multiply(0.1))
        .stroke(Stroke::new(1.0, col.gamma_multiply(0.35)))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                w::paint_icon(ui.painter(), r.shrink(2.0), if ok { Icon::Check } else { Icon::Info }, col);
                ui.add(egui::Label::new(RichText::new(text).size(12.5).color(theme::text())).wrap());
            });
        });
}
