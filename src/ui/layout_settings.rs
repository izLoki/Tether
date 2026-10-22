//! Settings → Layout: where each view lives (primary / secondary side bar, panel),
//! side bars and their icon strips, tabs and pane behaviour. A schematic of the window
//! at the top follows every change as it is made (and the app behind the dimmed
//! settings screen rearranges live too).

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use super::activity::{forget_sizes, paint_view_icon};
use super::settings_widgets::{self as w, Icon, SliderSpec, paint_icon};
use crate::keybindings::{self, Action};
use crate::layout::{BarOrientation, Container, ContainerState, LayoutSettings, Side, TabPosition, View};
use crate::settings::Settings;
use crate::theme;

const PREVIEW_H: f32 = 200.0;

/// Where a view is, as the settings list offers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    In(Container),
    Hidden,
}

pub fn show(ui: &mut Ui, s: &mut Settings, gap: f32) {
    let l = &mut s.layout;
    preview(ui, l);
    ui.add_space(gap);
    w::card(ui, Some("Views"), |ui| {
        ui.label(egui::RichText::new("Drag an icon onto another strip or a window edge to move it, or right-click it").size(11.5).color(theme::text_dim()));
        ui.add_space(4.0);
        for v in View::ALL {
            view_row(ui, l, v);
        }
        w::separator(ui);
        w::row(ui, "Reset layout", "Every view back in the primary side bar, on the left, default sizes", |ui| {
            if ui.button("Reset").clicked() {
                l.reset_views();
                forget_sizes(ui.ctx());
            }
        });
    });
    ui.add_space(gap);
    w::card(ui, Some("Side bars & panel"), |ui| {
        w::row(ui, "Primary side bar", "The side it docks on; the secondary side bar takes the other", |ui| {
            w::segmented(ui, &mut l.sidebar_side, &[(Side::Left, "Left"), (Side::Right, "Right")]);
        });
        for (c, action, what) in [
            (Container::Primary, Action::ToggleSidebar, "Show the primary side bar"),
            (Container::Secondary, Action::ToggleSecondarySidebar, "Show the secondary side bar"),
            (Container::Panel, Action::TogglePanel, "Show the panel below the terminals"),
        ] {
            w::separator(ui);
            let empty = l.views.get(c).is_empty();
            let desc = if empty { "Empty: move a view here to use it".to_string() } else { keybindings::hint(what, action) };
            ui.add_enabled_ui(!empty, |ui| {
                w::row(ui, &format!("{} open", c.title()), &desc, |ui| {
                    let mut open = !l.views.get(c).collapsed;
                    if w::toggle(ui, &mut open).changed() {
                        l.views.get_mut(c).collapsed = !open;
                    }
                });
            });
        }
        for c in [Container::Primary, Container::Secondary] {
            w::separator(ui);
            w::row(ui, &format!("{} icons", c.title()), "A strip on the outer edge, or a row along the top of the side bar", |ui| {
                w::segmented(ui, &mut l.views.get_mut(c).bar, &[(BarOrientation::Side, "Side"), (BarOrientation::Top, "Top")]);
            });
        }
    });
    ui.add_space(gap);
    w::card(ui, Some("Tabs"), |ui| {
        w::row(ui, "Tab position", "Vertical tabs in the Sessions view, or a tab strip on each pane", |ui| {
            w::segmented(ui, &mut l.tab_position, &[(TabPosition::Sidebar, "Sessions view"), (TabPosition::Top, "Top of panes")]);
        });
        if l.tab_position == TabPosition::Sidebar {
            ui.label(egui::RichText::new("Panes show their own tab strips while the Sessions view is hidden or folded").size(11.5).color(theme::text_faint()));
        }
        w::separator(ui);
        w::row(ui, "Group by pane", "Sessions view: a header for each split pane", |ui| {
            w::toggle(ui, &mut l.group_by_pane);
        });
        w::separator(ui);
        w::row(ui, "Compact rows", "Sessions view: one line per tab, no folder or address", |ui| {
            w::toggle(ui, &mut l.compact_tabs);
        });
    });
    ui.add_space(gap);
    panes_card(ui, l);
}

/// Icon, name and a location picker for one view.
fn view_row(ui: &mut Ui, l: &mut LayoutSettings, v: View) {
    let place = l.views.container_of(v).map_or(Place::Hidden, Place::In);
    ui.horizontal(|ui| {
        ui.set_min_height(40.0);
        let (r, _) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::hover());
        paint_view_icon(ui.painter(), r.shrink(3.0), v, theme::text_dim());
        ui.label(egui::RichText::new(v.title()).size(13.5).color(theme::text()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mut pick = place;
            let options = [
                (Place::In(Container::Primary), "Primary"),
                (Place::In(Container::Secondary), "Secondary"),
                (Place::In(Container::Panel), "Panel"),
                (Place::Hidden, "Hidden"),
            ];
            w::segmented(ui, &mut pick, &options);
            if pick != place {
                match pick {
                    Place::Hidden => l.views.hide(v),
                    Place::In(c) => l.views.move_view(v, c, None),
                }
            }
        });
    });
}

fn panes_card(ui: &mut Ui, l: &mut LayoutSettings) {
    w::card(ui, Some("Panes"), |ui| {
        w::row(ui, "Dim inactive panes", "Fade terminals that don't have focus when the window is split", |ui| {
            w::toggle(ui, &mut l.dim_inactive_panes);
        });
        ui.add_enabled_ui(l.dim_inactive_panes, |ui| {
            w::separator(ui);
            w::row(ui, "Dim amount", "", |ui| {
                let spec = SliderSpec { min: 0.1, max: LayoutSettings::DIM_MAX, step: 0.05, log: false, format: &|v| format!("{:.0}%", v * 100.0) };
                let mut v = l.dim_amount();
                if w::slider_with_value(ui, &mut v, &spec).changed() {
                    l.dim_amount = v;
                }
            });
        });
        w::separator(ui);
        w::row(ui, "Focus follows mouse", "Hovering a pane gives it the keyboard", |ui| {
            w::toggle(ui, &mut l.focus_follows_mouse);
        });
        w::separator(ui);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(egui::RichText::new("Shortcuts:").size(11.5).color(theme::text_dim()));
            for (what, action) in [
                ("primary side bar", Action::ToggleSidebar),
                ("secondary", Action::ToggleSecondarySidebar),
                ("panel", Action::TogglePanel),
                ("split", Action::SplitRight),
                ("maximize", Action::ZoomPane),
            ] {
                if let Some(b) = keybindings::binding(action) {
                    for cap in b.caps() {
                        w::key_chip(ui, &cap);
                    }
                    ui.label(egui::RichText::new(what).size(11.5).color(theme::text_faint()));
                    ui.add_space(8.0);
                }
            }
        });
    });
}

// ---- preview ----------------------------------------------------------------------

/// A small drawing of the window as the settings arrange it.
fn preview(ui: &mut Ui, l: &LayoutSettings) {
    let (outer, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), PREVIEW_H), Sense::hover());
    let p = ui.painter().clone();
    p.rect(outer, CornerRadius::same(12), theme::bg(), Stroke::new(1.0, theme::border().gamma_multiply(0.6)), egui::StrokeKind::Inside);
    let win = Rect::from_center_size(outer.center() + Vec2::new(0.0, 6.0), Vec2::new((outer.width() - 40.0).min(540.0), PREVIEW_H - 44.0));
    p.rect_filled(win.translate(Vec2::new(0.0, 4.0)), CornerRadius::same(8), theme::shadow().gamma_multiply(0.4));
    p.rect(win, CornerRadius::same(8), theme::surface(), Stroke::new(1.0, theme::border()), egui::StrokeKind::Inside);
    let ctx = ui.ctx().clone();
    let anim = |name: &str, v: f32| ctx.animate_value_with_time(egui::Id::new(("layout_preview", name)), v, theme::anim_time(0.22));

    let bar = Rect::from_min_size(win.min, Vec2::new(win.width(), 14.0));
    p.rect_filled(bar, CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 }, theme::surface_2());
    for (k, c) in [theme::danger(), theme::warn(), theme::accent()].into_iter().enumerate() {
        p.circle_filled(Pos2::new(bar.left() + 10.0 + k as f32 * 9.0, bar.center().y), 2.4, c.gamma_multiply(0.8));
    }
    let mut body = Rect::from_min_max(Pos2::new(win.left() + 3.0, bar.bottom() + 3.0), win.max - Vec2::splat(3.0));
    for side in [Side::Left, Side::Right] {
        let c = l.container_at(side);
        side_preview(&p, &mut body, l.views.get(c), side == Side::Right, &anim, c);
    }
    let panel = &l.views.panel;
    let panel_h = anim(
        "panel",
        if panel.is_empty() {
            0.0
        } else if panel.collapsed {
            11.0
        } else {
            body.height() * 0.36
        },
    );
    if panel_h > 0.5 {
        let r = Rect::from_min_max(Pos2::new(body.left(), body.bottom() - panel_h), body.max);
        body.max.y = r.top() - 3.0;
        p.rect_filled(r, CornerRadius::same(4), theme::surface_2());
        let mut x = r.left() + 5.0;
        for v in &panel.views {
            let on = panel.shown() == Some(*v);
            let col = if on { theme::text() } else { theme::text_faint() };
            let g = p.layout_no_wrap(v.title().to_uppercase(), FontId::proportional(6.5), col);
            let w = g.size().x;
            p.galley(Pos2::new(x, r.top() + 2.5), g, col);
            if on {
                p.hline(x..=x + w, r.top() + 10.0, Stroke::new(1.0, theme::accent()));
            }
            x += w + 8.0;
        }
        if !panel.collapsed && r.height() > 24.0 {
            content_lines(&p, Rect::from_min_max(Pos2::new(r.left(), r.top() + 14.0), r.max), panel.shown());
        }
    }
    panes(&p, body, l, &anim);
    paint_icon(&p, Rect::from_min_size(outer.left_top() + Vec2::new(12.0, 10.0), Vec2::splat(12.0)), Icon::Layout, theme::text_faint());
    p.text(outer.left_top() + Vec2::new(30.0, 16.0), Align2::LEFT_CENTER, "Preview", FontId::proportional(11.0), theme::text_faint());
}

/// A side bar (strip and body) cut off `body`'s left or right.
fn side_preview(p: &egui::Painter, body: &mut Rect, s: &ContainerState, right: bool, anim: &dyn Fn(&str, f32) -> f32, c: Container) {
    let key = |what: &str| format!("{what}{c:?}");
    let strip = !s.is_empty() && (s.bar == BarOrientation::Side || s.collapsed);
    let strip_w = anim(&key("strip"), if strip { 12.0 } else { 0.0 });
    if strip_w > 0.5 {
        let r = take(body, right, strip_w);
        p.rect_filled(r, CornerRadius::same(3), theme::surface_2().gamma_multiply(0.7));
        for (k, v) in s.views.iter().enumerate() {
            let col = if s.shown() == Some(*v) { theme::accent() } else { theme::text_faint() };
            let c = Pos2::new(r.center().x, r.top() + 7.0 + k as f32 * 11.0);
            p.rect_filled(Rect::from_center_size(c, Vec2::splat(6.0)), CornerRadius::same(2), col.gamma_multiply(strip_w / 12.0));
        }
        take(body, right, 2.0);
    }
    let open = !s.is_empty() && !s.collapsed;
    let w = anim(&key("bar"), if open { body.width() * 0.22 } else { 0.0 });
    if w <= 0.5 {
        return;
    }
    let r = take(body, right, w);
    take(body, right, 3.0);
    p.rect_filled(r, CornerRadius::same(4), theme::surface_2());
    let mut top = r.top() + 3.0;
    if s.bar == BarOrientation::Top {
        for (k, v) in s.views.iter().enumerate() {
            let col = if s.shown() == Some(*v) { theme::accent() } else { theme::text_faint() };
            let c = Pos2::new(r.left() + 7.0 + k as f32 * 10.0, top + 4.0);
            p.rect_filled(Rect::from_center_size(c, Vec2::splat(5.5)), CornerRadius::same(2), col);
        }
        top += 10.0;
    }
    if let Some(v) = s.shown() {
        p.text(Pos2::new(r.left() + 5.0, top + 4.0), Align2::LEFT_CENTER, v.title().to_uppercase(), FontId::proportional(6.5), theme::text_faint());
        content_lines(p, Rect::from_min_max(Pos2::new(r.left(), top + 9.0), r.max), Some(v));
    }
}

/// A view's body: tab rows for Sessions, text lines for the rest.
fn content_lines(p: &egui::Painter, r: Rect, view: Option<View>) {
    let mut y = r.top() + 2.0;
    for k in 0..5 {
        if y + 8.0 > r.bottom() {
            break;
        }
        if view == Some(View::Sessions) {
            let row = Rect::from_min_size(Pos2::new(r.left() + 3.0, y), Vec2::new(r.width() - 6.0, 9.0));
            if k == 0 {
                p.rect_filled(row, CornerRadius::same(2), theme::surface_3());
                p.rect_filled(Rect::from_min_size(row.min, Vec2::new(2.0, row.height())), CornerRadius::same(1), theme::accent());
            }
            p.rect_filled(
                Rect::from_center_size(Pos2::new(row.left() + 7.0, row.center().y), Vec2::splat(4.5)),
                CornerRadius::same(1),
                theme::hue_color(60.0 * k as f32 + 140.0),
            );
            p.rect_filled(
                Rect::from_min_size(Pos2::new(row.left() + 13.0, row.center().y - 1.5), Vec2::new(row.width() * 0.5, 3.0)),
                CornerRadius::same(1),
                theme::text_faint().gamma_multiply(0.6),
            );
            y += 11.0;
        } else {
            let line = Rect::from_min_size(Pos2::new(r.left() + 5.0, y), Vec2::new((r.width() - 10.0) * [0.8, 0.6, 0.7, 0.5, 0.65][k], 3.5));
            p.rect_filled(line, CornerRadius::same(2), theme::text_faint().gamma_multiply(0.35));
            y += 8.0;
        }
    }
}

/// Two panes, the left focused; tab strips unless vertical tabs are on screen.
fn panes(p: &egui::Painter, body: Rect, l: &LayoutSettings, anim: &dyn Fn(&str, f32) -> f32) {
    let panes = [
        Rect::from_min_max(body.min, Pos2::new(body.center().x - 1.5, body.bottom())),
        Rect::from_min_max(Pos2::new(body.center().x + 1.5, body.top()), body.max),
    ];
    let dim = anim("dim", if l.dim_inactive_panes { l.dim_amount() } else { 0.0 });
    let strips = anim("strips", if l.tab_bars_hidden() { 0.0 } else { 11.0 });
    for (k, pane) in panes.into_iter().enumerate() {
        let mut content = pane;
        if strips > 0.5 {
            let strip = Rect::from_min_size(pane.min, Vec2::new(pane.width(), strips));
            let tab = Rect::from_min_size(strip.min + Vec2::new(3.0, 2.0), Vec2::new(30.0, (strips - 3.0).max(0.0)));
            p.rect_filled(tab, CornerRadius::same(2), theme::surface_3());
            p.rect_filled(tab.translate(Vec2::new(33.0, 0.0)), CornerRadius::same(2), theme::surface_2());
            content = Rect::from_min_max(Pos2::new(pane.left(), strip.bottom() + 2.0), pane.max);
        }
        p.rect_filled(content, CornerRadius::same(4), theme::palette().term_bg);
        for line in 0..4 {
            let y = content.top() + 7.0 + line as f32 * 7.0;
            if y + 3.0 > content.bottom() {
                break;
            }
            let wfrac = [0.5, 0.75, 0.35, 0.6][line];
            let col = if line == 0 { theme::accent() } else { theme::text_dim() };
            p.rect_filled(
                Rect::from_min_size(Pos2::new(content.left() + 6.0, y), Vec2::new(content.width() * wfrac * 0.8, 3.0)),
                CornerRadius::same(1),
                col.gamma_multiply(0.6),
            );
        }
        if k == 0 {
            p.rect_stroke(content, CornerRadius::same(4), Stroke::new(1.0, theme::accent().gamma_multiply(0.7)), egui::StrokeKind::Inside);
        } else if dim > 0.0 {
            p.rect_filled(content, CornerRadius::same(4), theme::bg().gamma_multiply(dim));
        }
    }
    if l.focus_follows_mouse {
        // A pointer resting on the right pane.
        let tip = panes[1].center();
        let pts = vec![tip, tip + Vec2::new(0.0, 11.0), tip + Vec2::new(3.0, 8.0), tip + Vec2::new(7.5, 8.0)];
        p.add(egui::Shape::convex_polygon(pts, Color32::WHITE, Stroke::new(1.0, Color32::BLACK)));
    }
}

/// Cuts `w` points off the left (or right) of `r` and returns that strip.
fn take(r: &mut Rect, right: bool, w: f32) -> Rect {
    let w = w.min(r.width());
    if right {
        let strip = Rect::from_min_max(Pos2::new(r.right() - w, r.top()), r.max);
        r.max.x -= w;
        strip
    } else {
        let strip = Rect::from_min_max(r.min, Pos2::new(r.left() + w, r.bottom()));
        r.min.x += w;
        strip
    }
}
