//! "Generate a key" panel: name, algorithm family (with a size for ECDSA / RSA),
//! comment and an optional passphrase with a strength meter.

use eframe::egui::{self, CornerRadius, Rect, RichText, Sense, Ui, Vec2};

use super::{Done, KeyManager, Panel, spawn};
use crate::keystore::{self, KeyKind};
use crate::theme;
use crate::ui::form::{self, Kind};
use crate::ui::loading;
use crate::ui::settings_widgets::Icon;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Ed25519,
    Ecdsa,
    Rsa,
}

fn family(kind: KeyKind) -> Family {
    match kind {
        KeyKind::Ed25519 => Family::Ed25519,
        KeyKind::EcdsaP256 | KeyKind::EcdsaP384 | KeyKind::EcdsaP521 => Family::Ecdsa,
        KeyKind::Rsa2048 | KeyKind::Rsa3072 | KeyKind::Rsa4096 => Family::Rsa,
    }
}

/// The default kind when switching to `f`.
fn first_of(f: Family) -> KeyKind {
    match f {
        Family::Ed25519 => KeyKind::Ed25519,
        Family::Ecdsa => KeyKind::EcdsaP256,
        Family::Rsa => KeyKind::Rsa4096,
    }
}

impl KeyManager {
    pub(super) fn generate_panel(&mut self, ui: &mut Ui) {
        let busy = self.pending.is_some();
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(RichText::new("Generate a key").size(21.0).strong());
        ui.label(RichText::new("The private key never leaves this PC. Servers only get the public key.").size(12.5).color(theme::text_dim()));
        ui.add_space(16.0);
        let mut start = false;
        if let Panel::Generate(f) = &mut self.panel {
            ui.add_enabled_ui(!busy, |ui| {
                form::section(ui, Some(Icon::Key), "Key", "What it's called and how it's made", |ui| {
                    let width = ui.available_width();
                    let half = (width - form::GAP) / 2.0;
                    form::row2(ui, half, |ui, lw, rw| {
                        form::field(ui, lw, "Name", None, Some("Only shown in tether"), |ui| form::text(ui, &mut f.name, "e.g. Laptop 2026", lw, false));
                        form::field(ui, rw, "Comment", None, Some("Stored in the public key"), |ui| form::text(ui, &mut f.comment, "you@pc", rw, false));
                    });
                    ui.add_space(10.0);
                    form::label(ui, "Type", false);
                    ui.add_space(4.0);
                    let mut fam = family(f.kind);
                    form::segmented(
                        ui,
                        &mut fam,
                        &[(Family::Ed25519, "Ed25519", "recommended"), (Family::Ecdsa, "ECDSA", "NIST curves"), (Family::Rsa, "RSA", "old servers")],
                    );
                    if fam != family(f.kind) {
                        f.kind = first_of(fam);
                    }
                    if fam != Family::Ed25519 {
                        ui.add_space(8.0);
                        let shown = f.kind.label().to_string();
                        let mut pick = None;
                        let current = f.kind;
                        form::field(ui, width, "Size", None, None, |ui| {
                            form::select(ui, egui::Id::new("gen_kind"), width, &shown, |ui| {
                                for kind in KeyKind::ALL.into_iter().filter(|k| family(*k) == fam) {
                                    if form::option(ui, kind.label(), None, kind == current).clicked() {
                                        pick = Some(kind);
                                    }
                                }
                            });
                        });
                        if let Some(k) = pick {
                            f.kind = k;
                        }
                    }
                    ui.add_space(4.0);
                    form::note(ui, f.kind.hint(), false);
                });
                ui.add_space(14.0);
                form::section(ui, Some(Icon::Info), "Passphrase", "Optional, recommended: protects the key file if it's ever copied", |ui| {
                    let half = (ui.available_width() - form::GAP) / 2.0;
                    let mismatch = f.pass != f.pass2 && !f.pass2.is_empty();
                    form::row2(ui, half, |ui, lw, rw| {
                        form::field(ui, lw, "Passphrase", None, None, |ui| form::secret(ui, &mut f.pass, "optional", &mut f.show, lw, false));
                        let err = mismatch.then_some("Passphrases don't match");
                        form::field(ui, rw, "Repeat", err, None, |ui| form::secret(ui, &mut f.pass2, "", &mut f.show, rw, mismatch));
                    });
                    strength(ui, &f.pass);
                    if !f.pass.is_empty() {
                        ui.add_space(4.0);
                        ui.checkbox(&mut f.remember, format!("Remember in {} (no prompt when connecting)", crate::platform::credential_store()));
                    }
                });
            });
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let valid = f.pass == f.pass2;
                if busy {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
                    loading::mini_spinner(ui, r.center(), 7.0, theme::accent());
                    ui.label(RichText::new(format!("Generating {}…", f.kind.label())).color(theme::text_dim()));
                } else if form::button(ui, "Generate key", Kind::Primary, valid).clicked() {
                    start = true;
                }
            });
        }
        if start {
            self.start_generate();
        }
        self.message_line(ui);
    }

    fn start_generate(&mut self) {
        let Panel::Generate(f) = &self.panel else { return };
        let form = f.clone();
        // RSA 4096 takes seconds and encrypting runs the KDF; the form shows a spinner.
        self.pending = Some(spawn(move || {
            let key = keystore::generate(form.kind, &form.comment)?;
            let key = keystore::encrypt(key, Some(form.pass.as_str()))?;
            Ok(Done::Generated { form, key: Box::new(key) })
        }));
        self.message = None;
    }
}

/// Rough passphrase strength bar (length + character classes).
pub(super) fn strength(ui: &mut Ui, pass: &str) {
    if pass.is_empty() {
        return;
    }
    let classes = [
        pass.chars().any(|c| c.is_lowercase()),
        pass.chars().any(|c| c.is_uppercase()),
        pass.chars().any(|c| c.is_ascii_digit()),
        pass.chars().any(|c| !c.is_alphanumeric()),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    let score = ((pass.chars().count() as f32 / 16.0).min(1.0) * 0.7 + classes as f32 * 0.075).min(1.0);
    let (label, col) = match score {
        s if s < 0.4 => ("Weak", theme::danger()),
        s if s < 0.7 => ("Okay", theme::warn()),
        _ => ("Strong", theme::accent()),
    };
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new((ui.available_width() - 60.0).min(260.0), 5.0), Sense::hover());
        let k = ui.ctx().animate_value_with_time(resp.id, score, theme::anim_time(0.2));
        ui.painter().rect_filled(rect, CornerRadius::same(3), theme::surface_3());
        ui.painter().rect_filled(Rect::from_min_size(rect.min, Vec2::new(rect.width() * k, rect.height())), CornerRadius::same(3), col);
        ui.label(RichText::new(label).size(11.5).strong().color(col));
    });
}
