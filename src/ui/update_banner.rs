//! Update UI: a pill in the top bar when a newer release is out (with download
//! progress), and the "Updates" card of Settings → About.

use eframe::egui::{self, Align, CornerRadius, Layout, RichText, Sense, Stroke, Ui, Vec2};

use super::settings_widgets as w;
use crate::settings::Settings;
use crate::theme;
use crate::updater::{self, State};

const PILL_H: f32 = 24.0;

/// Top bar pill (call inside a right-to-left layout). Draws nothing while idle.
pub fn top_bar(ui: &mut Ui) {
    match updater::state() {
        State::Available(r) => {
            let resp = pill(ui, &format!("↑  Update v{} available  ·  {}", r.version, updater::install_label()), theme::accent(), None)
                .on_hover_text(updater::install_hint());
            if resp.clicked() {
                updater::install(ui.ctx());
            }
        }
        State::Downloading { version, got, total } => {
            let frac = if total > 0 { got as f32 / total as f32 } else { 0.0 };
            pill(ui, &format!("Downloading v{version}  {:.0}%", frac * 100.0), theme::blue(), Some(frac));
        }
        State::Installing => {
            pill(ui, "Installing update…", theme::blue(), Some(1.0));
        }
        State::InstallFailed(r, err) => {
            let resp = pill(ui, &format!("Update v{} failed  ·  Retry", r.version), theme::danger(), None).on_hover_text(err);
            if resp.clicked() {
                updater::install(ui.ctx());
            }
        }
        State::InstallerOpened(_) => {
            pill(ui, "Update opened · Drag Tether to Applications, then restart", theme::accent(), None);
        }
        State::Idle | State::Checking | State::UpToDate | State::Failed(_) => {}
    }
}

/// A rounded, clickable label tinted with `colour`; `progress` fills it from the left.
fn pill(ui: &mut Ui, text: &str, colour: egui::Color32, progress: Option<f32>) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), egui::FontId::proportional(11.5), colour);
    let size = Vec2::new(galley.size().x + 22.0, PILL_H);
    let sense = if progress.is_some() { Sense::hover() } else { Sense::click() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() && progress.is_none(), theme::anim_time(0.12));
    let radius = CornerRadius::same((PILL_H / 2.0) as u8);
    let p = ui.painter();
    p.rect(rect, radius, colour.gamma_multiply(0.12 + 0.1 * hover), Stroke::new(1.0, colour.gamma_multiply(0.5 + 0.4 * hover)), egui::StrokeKind::Inside);
    if let Some(frac) = progress {
        let mut fill = rect;
        fill.set_width(rect.width() * frac.clamp(0.0, 1.0));
        p.rect_filled(fill, radius, colour.gamma_multiply(0.22));
        ui.ctx().request_repaint();
    }
    p.galley(rect.center() - galley.size() / 2.0, galley, colour);
    if resp.hovered() && progress.is_none() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Settings → About → Updates.
pub fn about_card(ui: &mut Ui, s: &mut Settings) {
    w::card(ui, Some("Updates"), |ui| {
        w::row(ui, "Check for updates on startup", "Asks GitHub for a newer release when tether opens", |ui| {
            w::toggle(ui, &mut s.updates.check_on_startup);
        });
        w::separator(ui);
        let state = updater::state();
        let (status, colour) = status_text(&state);
        ui.horizontal(|ui| {
            ui.set_min_height(30.0);
            ui.label(RichText::new(status).size(12.5).color(colour));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| action_button(ui, &state));
        });
    });
}

fn status_text(state: &State) -> (String, egui::Color32) {
    let current = env!("CARGO_PKG_VERSION");
    match state {
        State::Idle => (format!("You're on v{current}"), theme::text_dim()),
        State::Checking => ("Checking GitHub…".into(), theme::text_dim()),
        State::UpToDate => (format!("v{current} is the latest version"), theme::accent()),
        State::Available(r) => (format!("v{} is available (you have v{current})", r.version), theme::accent()),
        State::Downloading { version, got, total } => (format!("Downloading v{version}: {} of {}", mb(*got), mb(*total)), theme::blue()),
        State::InstallerOpened(_) => ("Drag Tether to Applications, then restart Tether".into(), theme::accent()),
        State::Installing => ("Starting the installer…".into(), theme::blue()),
        State::Failed(e) => (format!("Couldn't check: {e}"), theme::warn()),
        State::InstallFailed(_, e) => (format!("Update failed: {e}"), theme::danger()),
    }
}

fn action_button(ui: &mut Ui, state: &State) {
    match state {
        State::Available(r) | State::InstallFailed(r, _) | State::InstallerOpened(r) => {
            if w::primary_button(ui, &format!("{} v{}", updater::install_label(), r.version)).clicked() {
                updater::install(ui.ctx());
            }
            if w::ghost_button(ui, "Release notes", theme::text_dim()).clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(&r.page));
            }
        }
        State::Checking | State::Downloading { .. } | State::Installing => {
            ui.add(egui::Spinner::new().size(16.0));
        }
        State::Idle | State::UpToDate | State::Failed(_) => {
            if w::ghost_button(ui, "Check now", theme::text()).clicked() {
                updater::check(ui.ctx());
            }
        }
    }
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}
