//! The editor's Authentication card: a large segmented control, then a password field,
//! saved keys as selectable cards (plus a "Generate new key" card), or a key file path.

use std::path::PathBuf;

use eframe::egui::{self, Align2, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use super::{AuthTab, EditorOutcome, ProfileEditor, default_key_path};
use crate::keystore::{KeyMeta, KeyStore};
use crate::profile::{AuthMethod, secret};
use crate::theme;
use crate::ui::form::{self, Kind};
use crate::ui::settings_widgets::{self as w, Icon};

const KEY_CARD_H: f32 = 64.0;

impl ProfileEditor {
    pub(super) fn auth_section(&mut self, ui: &mut Ui, keys: &KeyStore, outcome: &mut EditorOutcome) {
        let managed = self.auth_tab == AuthTab::Managed && !keys.keys.is_empty();
        let (manage, ()) = form::section_with(
            ui,
            Some(Icon::Key),
            "Authentication",
            "How tether proves it's you",
            |ui| managed && form::button(ui, "Manage keys", Kind::Ghost, true).clicked(),
            |ui| self.auth(ui, keys, outcome),
        );
        if manage {
            *outcome = EditorOutcome::ManageKeys;
        }
    }

    fn auth(&mut self, ui: &mut Ui, keys: &KeyStore, outcome: &mut EditorOutcome) {
        let before = self.auth_tab;
        form::segmented(
            ui,
            &mut self.auth_tab,
            &[
                (AuthTab::Password, "Password", "typed or remembered"),
                (AuthTab::Managed, "Saved key", "from the key manager"),
                (AuthTab::File, "Key file", "a file on this PC"),
            ],
        );
        if self.auth_tab != before {
            self.secret.clear();
            self.profile.auth = match self.auth_tab {
                AuthTab::Password => AuthMethod::Password,
                AuthTab::Managed => AuthMethod::Managed { key: keys.keys.first().map(|k| k.id).unwrap_or_default() },
                AuthTab::File => AuthMethod::Key { path: default_key_path() },
            };
            self.has_saved_secret = !self.is_new && secret::get(self.profile.secret_id()).is_some();
        }
        ui.add_space(16.0);
        match self.auth_tab {
            AuthTab::Password => {
                let help = if self.has_saved_secret {
                    "A password is saved. Leave blank to keep it."
                } else {
                    "Saved in the system credential store. Leave blank to be asked."
                };
                self.secret_field(ui, "Password", "", help);
            }
            AuthTab::Managed => self.managed_key(ui, keys, outcome),
            AuthTab::File => self.key_file(ui),
        }
    }

    fn key_file(&mut self, ui: &mut Ui) {
        let missing = self.tried_save && matches!(&self.profile.auth, AuthMethod::Key { path } if path.as_os_str().is_empty());
        if let AuthMethod::Key { path } = &mut self.profile.auth {
            let width = ui.available_width();
            form::field(
                ui,
                width,
                "Private key file",
                missing.then_some("Choose a private key file"),
                Some("OpenSSH or PEM format, e.g. ~/.ssh/id_ed25519"),
                |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let browse_w = 88.0;
                        let mut text = path.display().to_string();
                        if form::text(ui, &mut text, "C:\\Users\\you\\.ssh\\id_ed25519", width - browse_w - 8.0, missing).changed() {
                            *path = PathBuf::from(text);
                        }
                        if form::button(ui, "Browse…", Kind::Secondary, true).clicked()
                            && let Some(f) = rfd::FileDialog::new().set_title("Choose private key").pick_file()
                        {
                            *path = f;
                        }
                    });
                },
            );
        }
        if form::button(ui, "Search this PC for SSH keys…", Kind::Ghost, true).clicked() {
            self.key_search = Some(crate::ui::key_search::KeySearch::new(ui.ctx()));
        }
        if let Some(search) = &mut self.key_search {
            let (picked, close) = search.show(ui.ctx());
            if let Some(path) = picked {
                self.profile.auth = AuthMethod::Key { path };
            }
            if close {
                self.key_search = None;
            }
        }
        ui.add_space(10.0);
        self.secret_field(ui, "Passphrase", "only if the key has one", "Leave blank to be asked when connecting.");
    }

    fn managed_key(&mut self, ui: &mut Ui, keys: &KeyStore, outcome: &mut EditorOutcome) {
        if keys.keys.is_empty() {
            let half = (ui.available_width() - form::GAP) / 2.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = form::GAP;
                if action_card(ui, half, Icon::Plus, "Generate a new key", "Ed25519, ready in a second").clicked() {
                    *outcome = EditorOutcome::GenerateKey;
                }
                if action_card(ui, half, Icon::Folder, "Import a key file", "Add an existing ~/.ssh key").clicked() {
                    *outcome = EditorOutcome::ManageKeys;
                }
            });
            return;
        }
        let AuthMethod::Managed { key } = &mut self.profile.auth else { return };
        if keys.get(*key).is_none() {
            *key = keys.keys[0].id;
        }
        let mut picked = None;
        let col_w = (ui.available_width() - form::GAP) / 2.0;
        let cards = keys.keys.len() + 1;
        for row in 0..cards.div_ceil(2) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = form::GAP;
                for i in row * 2..(row * 2 + 2).min(cards) {
                    match keys.keys.get(i) {
                        Some(k) => {
                            if key_card(ui, col_w, k, *key == k.id).clicked() {
                                picked = Some(k.id);
                            }
                        }
                        None => {
                            if action_card(ui, col_w, Icon::Plus, "Generate new key", "Saved to the key manager").clicked() {
                                *outcome = EditorOutcome::GenerateKey;
                            }
                        }
                    }
                }
            });
            ui.add_space(form::GAP - 6.0);
        }
        if let Some(id) = picked
            && id != *key
        {
            *key = id;
            self.secret.clear();
            self.has_saved_secret = secret::get(id).is_some();
        }
        if keys.get(self.profile.secret_id()).is_some_and(|k| k.encrypted) {
            ui.add_space(4.0);
            let help = if self.has_saved_secret { "The passphrase is remembered. Leave blank to keep it." } else { "Leave blank to be asked when connecting." };
            self.secret_field(ui, "Key passphrase", "", help);
        }
    }

    fn secret_field(&mut self, ui: &mut Ui, label: &str, hint: &str, help: &str) {
        let hint = if self.has_saved_secret { "••••••••  saved" } else { hint };
        let width = ui.available_width();
        form::field(ui, width, label, None, Some(help), |ui| {
            form::secret(ui, &mut self.secret, hint, &mut self.show_secret, width, false);
        });
    }
}

/// Selectable key card: icon, name, algorithm badge and a short fingerprint.
fn key_card(ui: &mut Ui, width: f32, k: &KeyMeta, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, KEY_CARD_H), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, selected, &k.name));
    let resp = resp.on_hover_text(&k.fingerprint).on_hover_cursor(egui::CursorIcon::PointingHand);
    let (h, s) = card_anim(ui, &resp, selected);
    let p = ui.painter();
    paint_card(p, rect, h, s);
    let icon = Rect::from_center_size(Pos2::new(rect.left() + 30.0, rect.center().y), Vec2::splat(36.0));
    let tint = theme::accent().lerp_to_gamma(theme::text_dim(), 1.0 - s.max(0.5));
    p.rect_filled(icon, CornerRadius::same(9), tint.gamma_multiply(0.15));
    w::paint_icon(p, icon.shrink(10.0), Icon::Key, tint);
    let x = icon.right() + 12.0;
    let clip = p.with_clip_rect(Rect::from_min_max(Pos2::new(x, rect.top()), Pos2::new(rect.right() - 30.0, rect.bottom())));
    clip.text(Pos2::new(x, rect.top() + 22.0), Align2::LEFT_CENTER, &k.name, FontId::proportional(13.5), theme::text());
    let badge = p.layout_no_wrap(k.algorithm.clone(), FontId::proportional(10.5), theme::blue());
    let badge_rect = Rect::from_min_size(Pos2::new(x, rect.top() + 34.0), badge.size() + Vec2::new(12.0, 5.0));
    let badge_w = badge_rect.width();
    form::paint_tag(&clip, badge_rect, badge, theme::blue());
    let short: String = k.fingerprint.trim_start_matches("SHA256:").chars().take(10).collect();
    clip.text(Pos2::new(x + badge_w + 8.0, rect.top() + 42.0), Align2::LEFT_CENTER, format!("{short}…"), FontId::monospace(11.0), theme::text_faint());
    if k.encrypted {
        lock(p, Pos2::new(icon.right() - 3.0, icon.bottom() - 3.0));
    }
    radio(p, Pos2::new(rect.right() - 18.0, rect.top() + 18.0), s);
    resp
}

/// Dashed card that starts an action (generate / import).
fn action_card(ui: &mut Ui, width: f32, icon: Icon, title: &str, sub: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, KEY_CARD_H), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, title));
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let (h, _) = card_anim(ui, &resp, false);
    let p = ui.painter();
    if h > 0.0 {
        p.rect_filled(rect, CornerRadius::same(10), theme::accent().gamma_multiply(0.06 * h));
    }
    form::dashed(p, rect, theme::border().lerp_to_gamma(theme::accent(), 0.3 + 0.7 * h));
    let tile = Rect::from_center_size(Pos2::new(rect.left() + 30.0, rect.center().y), Vec2::splat(36.0));
    p.rect_filled(tile, CornerRadius::same(9), theme::accent().gamma_multiply(0.12 + 0.08 * h));
    w::paint_icon(p, tile.shrink(11.0), icon, theme::accent());
    let x = tile.right() + 12.0;
    p.text(Pos2::new(x, rect.top() + 24.0), Align2::LEFT_CENTER, title, FontId::proportional(13.5), theme::text());
    p.text(Pos2::new(x, rect.top() + 42.0), Align2::LEFT_CENTER, sub, FontId::proportional(11.5), theme::text_faint());
    resp
}

fn card_anim(ui: &Ui, resp: &egui::Response, selected: bool) -> (f32, f32) {
    let ctx = ui.ctx();
    (
        ctx.animate_bool_with_time(resp.id, resp.hovered(), theme::anim_time(0.1)),
        ctx.animate_bool_with_time(resp.id.with("sel"), selected, theme::anim_time(0.16)),
    )
}

fn paint_card(p: &egui::Painter, rect: Rect, hover: f32, sel: f32) {
    let fill = form::input_fill().lerp_to_gamma(theme::surface_3(), 0.35 * hover).lerp_to_gamma(theme::accent(), 0.07 * sel);
    let stroke = theme::border().lerp_to_gamma(theme::text_faint(), 0.4 * hover).lerp_to_gamma(theme::accent(), sel);
    if sel > 0.0 {
        p.rect_filled(rect.expand(3.0), CornerRadius::same(13), theme::accent().gamma_multiply(0.12 * sel));
    }
    p.rect(rect, CornerRadius::same(10), fill, Stroke::new(1.0 + 0.5 * sel, stroke), egui::StrokeKind::Inside);
}

/// Small padlock badge marking a key with a passphrase.
fn lock(p: &egui::Painter, c: Pos2) {
    p.circle_filled(c, 7.5, form::input_fill());
    let col = theme::warn();
    p.rect_filled(Rect::from_center_size(c + Vec2::new(0.0, 1.2), Vec2::new(7.0, 5.0)), CornerRadius::same(1), col);
    p.circle_stroke(c + Vec2::new(0.0, -1.6), 2.2, Stroke::new(1.2, col));
}

/// Radio dot: an empty ring that fills with the accent as `t` → 1.
fn radio(p: &egui::Painter, c: Pos2, t: f32) {
    p.circle_stroke(c, 7.0, Stroke::new(1.3, theme::border().lerp_to_gamma(theme::accent(), t)));
    if t > 0.0 {
        p.circle_filled(c, 7.0 * t, theme::accent());
        w::paint_icon(p, Rect::from_center_size(c, Vec2::splat(7.0)), Icon::Check, theme::on_accent().gamma_multiply(t));
    }
}
