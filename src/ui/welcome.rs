//! The screen shown while no tab is open: start a shell, reopen a recent host, and a
//! cheat-sheet of the shortcuts that matter most.

use eframe::egui::{self, Align2, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};

use super::settings_widgets::key_chip;
use crate::keybindings::{self, Action};
use crate::profile::Profile;
use crate::pty::ShellSpec;
use crate::theme;

const WIDTH: f32 = 520.0;
const RECENT: usize = 4;

pub enum WelcomeAction {
    Shell(&'static ShellSpec),
    Open(Profile),
    NewHost,
    Import,
    Palette,
}

pub fn show(ui: &mut Ui, profiles: &[Profile]) -> Option<WelcomeAction> {
    let mut out = None;
    let t = ui.ctx().animate_bool_with_time(egui::Id::new("welcome"), true, theme::anim_time(0.4));
    ui.set_opacity(t);
    egui::ScrollArea::vertical().id_salt("welcome_scroll").auto_shrink([false, false]).show(ui, |ui| {
        let width = WIDTH.min(ui.available_width() - 32.0);
        let top = ((ui.available_height() - 680.0) * 0.4).max(16.0);
        ui.vertical_centered(|ui| {
            ui.add_space(top + (1.0 - t) * 12.0);
            super::sidebar::logo(ui, 52.0);
            ui.label(RichText::new("tether").size(28.0).strong());
            ui.label(RichText::new("Local shells and SSH terminals, pipelined SFTP, side by side.").color(theme::text_dim()));
            ui.add_space(20.0);
            ui.allocate_ui(Vec2::new(width, 0.0), |ui| {
                ui.set_width(width);
                shells(ui, &mut out);
                ui.add_space(12.0);
                hosts(ui, profiles, &mut out);
                ui.add_space(12.0);
                cheat_sheet(ui, &mut out);
            });
            ui.add_space(16.0);
            ui.label(RichText::new("Drag tabs to split · drop files onto a remote pane to upload").size(11.5).color(theme::text_faint()));
            ui.add_space(16.0);
        });
    });
    out
}

fn card(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(theme::surface())
        .stroke(Stroke::new(1.0, theme::border().gamma_multiply(0.6)))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.label(RichText::new(title.to_uppercase()).size(10.5).color(theme::text_faint()));
                ui.add_space(8.0);
                add(ui);
            });
        });
}

fn shells(ui: &mut Ui, out: &mut Option<WelcomeAction>) {
    let Some(shells) = crate::pty::shells::detected() else {
        card(ui, "Start", |ui| {
            ui.label(RichText::new(crate::pty::shells::DETECTING).color(theme::text_dim()));
        });
        return;
    };
    let Some(default) = crate::pty::shells::pick_default(shells, &crate::settings::get().terminal.default_shell) else { return };
    card(ui, "Start", |ui| {
        let text = format!("{}  New {}", default.icon_kind.glyph(), default.name);
        if wide_button(ui, &text, keybindings::label(Action::NewLocalTerminal), true)
            .on_hover_text(keybindings::hint("Local terminal", Action::NewLocalTerminal))
            .clicked()
        {
            *out = Some(WelcomeAction::Shell(default));
        }
        let others: Vec<&'static ShellSpec> = shells.iter().filter(|s| s.id != default.id).collect();
        if !others.is_empty() {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                for s in others {
                    let chip = egui::Button::new(RichText::new(format!("{}  {}", s.icon_kind.glyph(), s.name)).size(12.0))
                        .fill(theme::surface_2())
                        .corner_radius(CornerRadius::same(7));
                    if ui.add(chip).clicked() {
                        *out = Some(WelcomeAction::Shell(s));
                    }
                }
            });
        }
    });
}

fn hosts(ui: &mut Ui, profiles: &[Profile], out: &mut Option<WelcomeAction>) {
    let mut recent: Vec<&Profile> = profiles.iter().filter(|p| p.last_used > 0 && !p.archived).collect();
    recent.sort_by_key(|p| std::cmp::Reverse(p.last_used));
    card(ui, if recent.is_empty() { "Hosts" } else { "Recent hosts" }, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        for p in recent.into_iter().take(RECENT) {
            if host_row(ui, p).clicked() {
                *out = Some(WelcomeAction::Open(p.clone()));
            }
        }
        ui.add_space(6.0);
        ui.columns(2, |cols| {
            if cols[0]
                .add(egui::Button::new(RichText::new("+  New host").size(12.5)).min_size(Vec2::new(cols[0].available_width(), 32.0)).fill(theme::accent_soft()))
                .clicked()
            {
                *out = Some(WelcomeAction::NewHost);
            }
            if cols[1]
                .add(egui::Button::new(RichText::new("⬇  Import from Bitvise / SSH config").size(12.5)).min_size(Vec2::new(cols[1].available_width(), 32.0)))
                .clicked()
            {
                *out = Some(WelcomeAction::Import);
            }
        });
    });
}

fn host_row(ui: &mut Ui, p: &Profile) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), Sense::click());
    let label = format!("⌨  {}   ·   {}", p.display_name(), p.address());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), theme::anim_time(0.12));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let paint = ui.painter();
    paint.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::surface_2().gamma_multiply(0.35 + 0.65 * h));
    let tint = theme::hue_color(p.hue);
    let tile = Rect::from_center_size(Pos2::new(rect.left() + 20.0, rect.center().y), Vec2::splat(24.0));
    paint.rect_filled(tile, CornerRadius::same(6), tint.gamma_multiply(0.2));
    let initial = p.display_name().chars().next().unwrap_or('?').to_uppercase().to_string();
    paint.text(tile.center(), Align2::CENTER_CENTER, initial, FontId::proportional(12.0), tint);
    paint.text(Pos2::new(tile.right() + 10.0, rect.center().y), Align2::LEFT_CENTER, p.display_name(), FontId::proportional(13.0), theme::text());
    paint.text(
        Pos2::new(rect.right() - 12.0, rect.center().y),
        Align2::RIGHT_CENTER,
        p.address(),
        FontId::proportional(11.5),
        theme::text_faint().lerp_to_gamma(theme::text_dim(), h),
    );
    resp
}

/// A full-width button with a shortcut on its right.
fn wide_button(ui: &mut Ui, text: &str, keys: String, primary: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, text));
    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), theme::anim_time(0.12));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let p = ui.painter();
    let fill = if primary { theme::accent_soft().lerp_to_gamma(theme::accent().gamma_multiply(0.35), h) } else { theme::surface_2() };
    p.rect_filled(rect, CornerRadius::same(theme::RADIUS), fill);
    p.text(Pos2::new(rect.left() + 14.0, rect.center().y), Align2::LEFT_CENTER, text, FontId::proportional(13.5), theme::text());
    if !keys.is_empty() {
        p.text(Pos2::new(rect.right() - 14.0, rect.center().y), Align2::RIGHT_CENTER, keys, FontId::monospace(11.0), theme::text_dim());
    }
    resp
}

fn cheat_sheet(ui: &mut Ui, out: &mut Option<WelcomeAction>) {
    card(ui, "Shortcuts", |ui| {
        let items: [(&str, Option<String>); 8] = [
            ("Command palette", keybindings::binding(Action::CommandPalette).map(|b| b.label())),
            ("New terminal", keybindings::binding(Action::NewLocalTerminal).map(|b| b.label())),
            ("Split right", keybindings::binding(Action::SplitRight).map(|b| b.label())),
            ("Split down", keybindings::binding(Action::SplitDown).map(|b| b.label())),
            ("Move between panes", focus_keys()),
            ("Maximize pane", keybindings::binding(Action::ZoomPane).map(|b| b.label())),
            ("Toggle sidebar", keybindings::binding(Action::ToggleSidebar).map(|b| b.label())),
            ("Settings", keybindings::binding(Action::OpenSettings).map(|b| b.label())),
        ];
        egui::Grid::new("welcome_keys").num_columns(4).spacing(Vec2::new(10.0, 8.0)).show(ui, |ui| {
            for (i, (what, keys)) in items.iter().enumerate() {
                ui.label(RichText::new(*what).size(12.0).color(theme::text_dim()));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    match keys {
                        Some(k) => k.split('+').for_each(|cap| key_chip(ui, cap)),
                        None => {
                            ui.label(RichText::new("unbound").size(11.0).color(theme::text_faint()));
                        }
                    }
                });
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
        ui.add_space(6.0);
        let link =
            ui.add(egui::Label::new(RichText::new("Everything else is in the command palette  ›").size(11.5).color(theme::accent())).sense(Sense::click()));
        if link.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if link.clicked() {
            *out = Some(WelcomeAction::Palette);
        }
    });
}

/// "Alt+Shift+←→↑↓" when the four focus bindings share their modifiers.
fn focus_keys() -> Option<String> {
    let all = [Action::FocusPaneLeft, Action::FocusPaneRight, Action::FocusPaneUp, Action::FocusPaneDown].map(keybindings::binding);
    let first = all[0].clone()?;
    let same = all.iter().all(|b| b.as_ref().is_some_and(|b| b.ctrl == first.ctrl && b.alt == first.alt && b.shift == first.shift));
    if !same {
        return Some(first.label());
    }
    let mut caps = first.caps();
    caps.pop();
    caps.push("←→↑↓".into());
    Some(caps.join("+"))
}
