//! Painted building blocks for the Settings screen: cards, rows, toggle switch,
//! segmented control, slider, key chips and line icons (no font glyphs needed).

use eframe::egui::{self, Align, Color32, CornerRadius, FontId, Layout, Pos2, Rect, Response, RichText, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::theme;

pub const CARD_PAD: i8 = 18;

fn hand_on_hover(ui: &Ui, resp: &Response) {
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
}

/// A rounded section card with an optional small heading.
pub fn card<R>(ui: &mut Ui, title: Option<&str>, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(theme::surface_2())
        .stroke(Stroke::new(1.0, theme::border().gamma_multiply(0.5)))
        .corner_radius(12)
        .inner_margin(egui::Margin::symmetric(CARD_PAD, 16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(t) = title {
                ui.label(RichText::new(t.to_uppercase()).size(10.5).strong().color(theme::text_faint()));
                ui.add_space(8.0);
            }
            add(ui)
        })
        .inner
}

/// Title + description on the left, `control` right-aligned.
pub fn row(ui: &mut Ui, title: &str, desc: &str, control: impl FnOnce(&mut Ui)) {
    let labels = |ui: &mut Ui| {
        ui.add_space(2.0);
        ui.label(RichText::new(title).size(13.5).color(theme::text()));
        if !desc.is_empty() {
            ui.label(RichText::new(desc).size(11.5).color(theme::text_dim()));
        }
    };
    if ui.available_width() < 520.0 {
        ui.vertical(|ui| {
            labels(ui);
            ui.add_space(4.0);
            control(ui);
        });
    } else {
        ui.horizontal(|ui| {
            ui.set_min_height(40.0);
            ui.vertical(labels);
            ui.with_layout(Layout::right_to_left(Align::Center), control);
        });
    }
}

/// Hairline between rows inside a card.
pub fn separator(ui: &mut Ui) {
    ui.add_space(4.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0, theme::border().gamma_multiply(0.45)));
    ui.add_space(4.0);
}

/// iOS-style switch with an animated knob.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(38.0, 22.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let value = *on;
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, value, ""));
    hand_on_hover(ui, &resp);
    let t = ui.ctx().animate_bool_with_time(resp.id, value, theme::anim_time(0.16));
    let hover = ui.ctx().animate_bool_with_time(resp.id.with("h"), resp.hovered(), theme::anim_time(0.1));
    let off = theme::surface_3().lerp_to_gamma(theme::border(), 0.5 + 0.5 * hover);
    let track = off.lerp_to_gamma(theme::accent(), t);
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(11), track);
    let x = egui::lerp((rect.left() + 11.0)..=(rect.right() - 11.0), t);
    let knob = Pos2::new(x, rect.center().y);
    p.circle_filled(knob + Vec2::new(0.0, 1.0), 8.0, theme::shadow().gamma_multiply(0.5));
    let knob_fill = if theme::palette().dark { theme::text_dim().lerp_to_gamma(Color32::WHITE, t) } else { Color32::WHITE };
    p.circle_filled(knob, 8.0, knob_fill);
    resp
}

/// Equal-width segmented control with a sliding selection pill.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> Response {
    let font = FontId::proportional(12.5);
    let widest = options.iter().map(|(_, l)| ui.painter().layout_no_wrap((*l).to_string(), font.clone(), theme::text()).size().x).fold(0.0_f32, f32::max);
    let seg_w = (widest + 26.0).max(56.0);
    let size = Vec2::new(seg_w * options.len() as f32 + 6.0, 30.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::hover());
    let selected = options.iter().position(|(v, _)| v == value).unwrap_or(0);
    let pos = ui.ctx().animate_value_with_time(resp.id.with("pill"), selected as f32, theme::anim_time(0.2));
    let p = ui.painter().clone();
    p.rect_filled(rect, CornerRadius::same(9), theme::surface_3().gamma_multiply(if theme::palette().dark { 1.0 } else { 0.9 }));
    let seg = |i: f32| Rect::from_min_size(Pos2::new(rect.left() + 3.0 + i * seg_w, rect.top() + 3.0), Vec2::new(seg_w, size.y - 6.0));
    let pill = seg(pos);
    p.rect_filled(pill.translate(Vec2::new(0.0, 1.0)), CornerRadius::same(7), theme::shadow().gamma_multiply(0.35));
    p.rect_filled(pill, CornerRadius::same(7), theme::surface());
    for (i, (v, label)) in options.iter().enumerate() {
        let r = seg(i as f32);
        let item = ui.interact(r, resp.id.with(i), Sense::click());
        hand_on_hover(ui, &item);
        if item.clicked() && *v != *value {
            *value = *v;
            resp.mark_changed();
        }
        let active = i == selected;
        let colour = if active || item.hovered() { theme::text() } else { theme::text_dim() };
        p.text(r.center(), egui::Align2::CENTER_CENTER, *label, font.clone(), colour);
    }
    resp
}

/// Painted slider. `log` maps the track logarithmically (for wide ranges).
pub struct SliderSpec<'a> {
    pub min: f32,
    pub max: f32,
    pub step: f32,
    pub log: bool,
    pub format: &'a dyn Fn(f32) -> String,
}

pub fn slider(ui: &mut Ui, value: &mut f32, spec: &SliderSpec) -> Response {
    let width = 200.0_f32.min(ui.available_width() - 70.0).max(80.0);
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::click_and_drag());
    hand_on_hover(ui, &resp);
    let to_t = |v: f32| {
        if spec.log { (v.max(spec.min).ln() - spec.min.ln()) / (spec.max.ln() - spec.min.ln()) } else { (v - spec.min) / (spec.max - spec.min) }.clamp(0.0, 1.0)
    };
    let from_t = |t: f32| if spec.log { (spec.min.ln() + t * (spec.max.ln() - spec.min.ln())).exp() } else { spec.min + t * (spec.max - spec.min) };
    let track = Rect::from_center_size(rect.center(), Vec2::new(rect.width() - 16.0, 4.0));
    if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
        let t = ((p.x - track.left()) / track.width()).clamp(0.0, 1.0);
        let snapped = snap(from_t(t), spec.step).clamp(spec.min, spec.max);
        if snapped != *value {
            *value = snapped;
            resp.mark_changed();
        }
    }
    let t = to_t(*value);
    let hot = ui.ctx().animate_bool_with_time(resp.id.with("hot"), resp.hovered() || resp.dragged(), theme::anim_time(0.12));
    let p = ui.painter();
    p.rect_filled(track, CornerRadius::same(2), theme::surface_3().lerp_to_gamma(theme::border(), 0.6));
    let filled = Rect::from_min_max(track.min, Pos2::new(track.left() + track.width() * t, track.bottom()));
    p.rect_filled(filled, CornerRadius::same(2), theme::accent());
    let knob = Pos2::new(filled.right(), track.center().y);
    let r = 7.0 + 1.5 * hot;
    p.circle_filled(knob, r + 5.0 * hot, theme::accent().gamma_multiply(0.18 * hot));
    p.circle_filled(knob + Vec2::new(0.0, 1.0), r, theme::shadow().gamma_multiply(0.5));
    p.circle(knob, r, Color32::WHITE, Stroke::new(2.0, theme::accent()));
    let shown = *value;
    resp.widget_info(|| egui::WidgetInfo::slider(true, shown as f64, ""));
    resp
}

pub fn snap(v: f32, step: f32) -> f32 {
    if step > 0.0 { (v / step).round() * step } else { v }
}

/// Slider plus a fixed-width value readout, laid out right-to-left inside a `row`.
pub fn slider_with_value(ui: &mut Ui, value: &mut f32, spec: &SliderSpec) -> Response {
    let text = (spec.format)(*value);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(64.0, 24.0), Sense::hover());
    ui.painter().text(rect.right_center(), egui::Align2::RIGHT_CENTER, text, FontId::monospace(12.0), theme::text_dim());
    slider(ui, value, spec)
}

/// Accent filled button.
pub fn primary_button(ui: &mut Ui, label: &str) -> Response {
    let text = RichText::new(label).color(theme::on_accent()).size(12.5).strong();
    let resp = ui.add(egui::Button::new(text).fill(theme::accent()).corner_radius(8).min_size(Vec2::new(64.0, 28.0)));
    hand_on_hover(ui, &resp);
    resp
}

pub fn ghost_button(ui: &mut Ui, label: &str, colour: Color32) -> Response {
    let resp =
        ui.add(egui::Button::new(RichText::new(label).color(colour).size(12.5)).fill(theme::surface_3()).corner_radius(8).min_size(Vec2::new(64.0, 28.0)));
    hand_on_hover(ui, &resp);
    resp
}

/// Round icon button showing a painted X. `danger` tints it red on hover.
pub fn close_button(ui: &mut Ui, size: f32, danger: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    hand_on_hover(ui, &resp);
    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), theme::anim_time(0.1));
    let p = ui.painter();
    if h > 0.0 {
        let fill = if danger { theme::danger().gamma_multiply(0.18) } else { theme::surface_3() };
        p.circle_filled(rect.center(), size * 0.5, fill.gamma_multiply(h));
    }
    let colour = if danger && h > 0.5 { theme::danger() } else { theme::text_dim().lerp_to_gamma(theme::text(), h) };
    paint_icon(p, rect.shrink(size * 0.3), Icon::Close, colour);
    resp
}

/// Small rounded "key" chip, e.g. `Ctrl`.
pub fn key_chip(ui: &mut Ui, key: &str) {
    let font = FontId::monospace(11.0);
    let galley = ui.painter().layout_no_wrap(key.to_string(), font, theme::text());
    let size = galley.size() + Vec2::new(12.0, 6.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect.translate(Vec2::new(0.0, 1.5)), CornerRadius::same(5), theme::border());
    p.rect(rect, CornerRadius::same(5), theme::surface_3(), Stroke::new(1.0, theme::border()), StrokeKind::Inside);
    p.galley(rect.min + Vec2::new(6.0, 3.0), galley, theme::text());
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Appearance,
    Terminal,
    Folder,
    Editor,
    FileType,
    Info,
    Close,
    Check,
    Gear,
    Keyboard,
    Key,
    Server,
    Plus,
    /// Panes in a split arrangement (layout settings, workspaces).
    Layout,
    /// A sparkle (AI agents).
    Sparkle,
    /// Magnifier (command palette, search).
    Search,
    /// A window with a side panel; `SidebarRight` mirrors it.
    SidebarLeft,
    SidebarRight,
    /// Stacked rows (a tab list).
    Rows,
    /// Three lines (the app menu).
    Menu,
    /// A mast with waves on both sides (broadcast input).
    Broadcast,
}

/// Frameless icon button (e.g. the gear that opens Settings in the top bar).
pub fn icon_button(ui: &mut Ui, icon: Icon, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    hand_on_hover(ui, &resp);
    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), theme::anim_time(0.1));
    if h > 0.0 {
        ui.painter().rect_filled(rect, CornerRadius::same(6), theme::surface_3().gamma_multiply(h));
    }
    let colour = theme::text_dim().lerp_to_gamma(theme::text(), h);
    paint_icon(ui.painter(), rect.shrink(size * 0.22), icon, colour);
    resp
}

/// Line icons painted inside `r` (square works best).
pub fn paint_icon(p: &egui::Painter, r: Rect, icon: Icon, c: Color32) {
    let w = (r.width() / 10.0).clamp(1.2, 1.8);
    let s = Stroke::new(w, c);
    let at = |x: f32, y: f32| Pos2::new(r.left() + x * r.width(), r.top() + y * r.height());
    match icon {
        Icon::Appearance => {
            let centre = r.center();
            let rad = r.width() * 0.42;
            p.circle_stroke(centre, rad, s);
            // Half-filled disc: the classic "contrast" glyph.
            let pts: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / 16.0;
                    centre + Vec2::new(a.cos(), a.sin()) * (rad - w * 0.5)
                })
                .collect();
            p.add(egui::Shape::convex_polygon(pts, c, Stroke::NONE));
        }
        Icon::Terminal => {
            p.rect_stroke(Rect::from_min_max(at(0.05, 0.12), at(0.95, 0.88)), CornerRadius::same(3), s, StrokeKind::Middle);
            p.line_segment([at(0.24, 0.36), at(0.42, 0.5)], s);
            p.line_segment([at(0.42, 0.5), at(0.24, 0.64)], s);
            p.line_segment([at(0.5, 0.66), at(0.74, 0.66)], s);
        }
        Icon::Folder => {
            let pts = vec![at(0.06, 0.22), at(0.38, 0.22), at(0.48, 0.34), at(0.94, 0.34), at(0.94, 0.84), at(0.06, 0.84), at(0.06, 0.22)];
            p.add(egui::Shape::line(pts, s));
        }
        Icon::Editor => {
            for (y, len) in [(0.22, 0.86), (0.42, 0.66), (0.62, 0.8), (0.82, 0.5)] {
                p.line_segment([at(0.08, y), at(0.08 + len, y)], s);
            }
        }
        Icon::FileType => {
            let pts = vec![at(0.2, 0.06), at(0.6, 0.06), at(0.84, 0.3), at(0.84, 0.94), at(0.2, 0.94), at(0.2, 0.06)];
            p.add(egui::Shape::line(pts, s));
            p.add(egui::Shape::line(vec![at(0.6, 0.06), at(0.6, 0.3), at(0.84, 0.3)], s));
        }
        Icon::Info => {
            p.circle_stroke(r.center(), r.width() * 0.44, s);
            p.circle_filled(at(0.5, 0.3), w * 0.9, c);
            p.line_segment([at(0.5, 0.45), at(0.5, 0.74)], s);
        }
        Icon::Close => {
            p.line_segment([r.left_top(), r.right_bottom()], s);
            p.line_segment([r.right_top(), r.left_bottom()], s);
        }
        Icon::Gear => {
            let c = r.center();
            let outer = r.width() * 0.46;
            let inner = r.width() * 0.34;
            let teeth = 8;
            let pts: Vec<Pos2> = (0..teeth * 4)
                .map(|i| {
                    let a = std::f32::consts::TAU * (i as f32 - 0.5) / (teeth * 4) as f32;
                    let rad = if (i / 2) % 2 == 0 { outer } else { inner };
                    c + Vec2::new(a.cos(), a.sin()) * rad
                })
                .collect();
            p.add(egui::Shape::closed_line(pts, s));
            p.circle_stroke(c, r.width() * 0.14, s);
        }
        Icon::Keyboard => {
            p.rect_stroke(Rect::from_min_max(at(0.02, 0.2), at(0.98, 0.8)), CornerRadius::same(3), s, StrokeKind::Middle);
            for row in [0.38, 0.52] {
                for col in [0.2, 0.38, 0.56, 0.74] {
                    p.circle_filled(at(col + 0.03, row), w * 0.55, c);
                }
            }
            p.line_segment([at(0.3, 0.66), at(0.7, 0.66)], s);
        }
        Icon::Key => {
            let ring = at(0.3, 0.5);
            p.circle_stroke(ring, r.width() * 0.22, s);
            p.line_segment([at(0.52, 0.5), at(0.96, 0.5)], s);
            p.line_segment([at(0.84, 0.5), at(0.84, 0.72)], s);
            p.line_segment([at(0.66, 0.5), at(0.66, 0.66)], s);
        }
        Icon::Server => {
            for (top, bottom) in [(0.1, 0.45), (0.55, 0.9)] {
                p.rect_stroke(Rect::from_min_max(at(0.06, top), at(0.94, bottom)), CornerRadius::same(2), s, StrokeKind::Middle);
                p.circle_filled(at(0.24, (top + bottom) / 2.0), w * 0.7, c);
                p.line_segment([at(0.5, (top + bottom) / 2.0), at(0.78, (top + bottom) / 2.0)], s);
            }
        }
        Icon::Plus => {
            p.line_segment([at(0.5, 0.1), at(0.5, 0.9)], s);
            p.line_segment([at(0.1, 0.5), at(0.9, 0.5)], s);
        }
        Icon::Check => {
            p.add(egui::Shape::line(vec![at(0.12, 0.52), at(0.4, 0.78), at(0.88, 0.24)], s));
        }
        Icon::Layout => {
            p.rect_stroke(Rect::from_min_max(at(0.06, 0.1), at(0.94, 0.9)), CornerRadius::same(3), s, StrokeKind::Middle);
            p.line_segment([at(0.5, 0.1), at(0.5, 0.9)], s);
            p.line_segment([at(0.5, 0.5), at(0.94, 0.5)], s);
        }
        Icon::Sparkle => {
            let star = |cx: f32, cy: f32, k: f32| {
                let c0 = at(cx, cy);
                let (a, b) = (r.width() * 0.36 * k, r.width() * 0.09 * k);
                let pts = vec![
                    c0 + Vec2::new(0.0, -a),
                    c0 + Vec2::new(b, -b),
                    c0 + Vec2::new(a, 0.0),
                    c0 + Vec2::new(b, b),
                    c0 + Vec2::new(0.0, a),
                    c0 + Vec2::new(-b, b),
                    c0 + Vec2::new(-a, 0.0),
                    c0 + Vec2::new(-b, -b),
                ];
                p.add(egui::Shape::closed_line(pts, s));
            };
            star(0.42, 0.56, 1.0);
            star(0.8, 0.2, 0.45);
        }
        Icon::Search => {
            p.circle_stroke(at(0.42, 0.42), r.width() * 0.3, s);
            p.line_segment([at(0.64, 0.64), at(0.92, 0.92)], s);
        }
        Icon::SidebarLeft | Icon::SidebarRight => {
            p.rect_stroke(Rect::from_min_max(at(0.04, 0.14), at(0.96, 0.86)), CornerRadius::same(3), s, StrokeKind::Middle);
            let x = if icon == Icon::SidebarLeft { 0.38 } else { 0.62 };
            p.line_segment([at(x, 0.14), at(x, 0.86)], s);
            let (a, b) = if icon == Icon::SidebarLeft { (0.14, 0.28) } else { (0.72, 0.86) };
            for y in [0.34, 0.5] {
                p.line_segment([at(a, y), at(b, y)], Stroke::new(w * 0.8, c));
            }
        }
        Icon::Menu => {
            for y in [0.24, 0.5, 0.76] {
                p.line_segment([at(0.12, y), at(0.88, y)], s);
            }
        }
        Icon::Broadcast => {
            let c0 = at(0.5, 0.5);
            p.circle_filled(c0, w * 1.2, c);
            for (rad, sweep) in [(0.27, 1.5), (0.47, 1.3)] {
                for side in [0.0, std::f32::consts::PI] {
                    let pts: Vec<Pos2> = (0..=8)
                        .map(|i| {
                            let a = side - sweep / 2.0 + sweep * i as f32 / 8.0;
                            c0 + Vec2::new(a.cos(), a.sin()) * r.width() * rad
                        })
                        .collect();
                    p.add(egui::Shape::line(pts, s));
                }
            }
        }
        Icon::Rows => {
            for y in [0.2, 0.5, 0.8] {
                p.rect_stroke(Rect::from_min_max(at(0.06, y - 0.11), at(0.94, y + 0.11)), CornerRadius::same(2), s, StrokeKind::Middle);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_rounds_to_step() {
        assert_eq!(snap(13.26, 0.5), 13.5);
        assert!((snap(1.13, 0.05) - 1.15).abs() < 1e-5);
        assert_eq!(snap(7.0, 0.0), 7.0);
    }
}
