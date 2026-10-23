//! Right-hand panel of the key manager for the selected key: public key, fingerprint,
//! install on a host, and the edit / passphrase / export / delete actions with their
//! inline forms.

use eframe::egui::{self, RichText, Sense, Ui, Vec2};
use uuid::Uuid;

use super::{Done, Form, KeyManager, KeyOutcome, Panel, Remember, date, file_stem, spawn, uses_key};
use crate::keystore::{self, KeyMeta, KeyStore};
use crate::profile::{ProfileStore, secret};
use crate::theme;
use crate::ui::form::{self, Kind};
use crate::ui::loading;
use crate::ui::settings_widgets::{self as w, Icon};

impl KeyManager {
    pub(super) fn details(&mut self, ui: &mut Ui, keys: &mut KeyStore, profiles: &ProfileStore, outcome: &mut KeyOutcome) {
        ui.spacing_mut().item_spacing.y = 6.0;
        let Some(k) = self.selected.and_then(|id| keys.get(id)).cloned() else {
            self.empty_state(ui, keys, outcome);
            self.message_line(ui);
            return;
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(&k.name).size(21.0).strong());
            ui.add_space(4.0);
            form::tag(ui, &k.algorithm, theme::blue());
            if k.encrypted {
                form::tag(ui, "passphrase", theme::warn());
            } else {
                form::tag(ui, "no passphrase", theme::text_dim());
            }
        });
        let comment = if k.comment.is_empty() { "No comment".to_string() } else { k.comment.clone() };
        ui.label(RichText::new(format!("{comment}  ·  created {}", date(k.created))).size(12.0).color(theme::text_faint()));
        self.message_line(ui);
        ui.add_space(16.0);

        form::section(ui, Some(Icon::Key), "Public key", "Safe to share: servers list it in ~/.ssh/authorized_keys", |ui| {
            let mut text = k.public.clone();
            let bg = ui.painter().add(egui::Shape::Noop);
            let resp = ui.add(
                egui::TextEdit::multiline(&mut text)
                    .font(egui::FontId::monospace(12.0))
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .frame(egui::Frame::new().inner_margin(egui::Margin::same(12))),
            );
            form::paint_frame(ui, bg, resp.id, resp.rect, resp.has_focus(), resp.hovered(), false);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if form::button(ui, "Copy public key", Kind::Primary, true).clicked() {
                    ui.ctx().copy_text(k.public.clone());
                    self.message = Some((true, "Public key copied. Paste it into ~/.ssh/authorized_keys on the server.".into()));
                }
                if form::button(ui, "Save .pub…", Kind::Secondary, true).clicked()
                    && let Some(path) = rfd::FileDialog::new().set_file_name(format!("{}.pub", file_stem(&k.name))).save_file()
                {
                    self.message = Some(match std::fs::write(&path, format!("{}\n", k.public)) {
                        Ok(()) => (true, format!("Saved {}", path.display())),
                        Err(e) => (false, format!("Saving failed: {e}")),
                    });
                }
            });
        });
        ui.add_space(14.0);
        form::section(ui, Some(Icon::Info), "Fingerprint", "Compare it with what the server shows", |ui| fingerprint(ui, &k, profiles));
        ui.add_space(14.0);
        form::section(ui, Some(Icon::Server), "Use on a server", "Appends the public key to ~/.ssh/authorized_keys over SSH", |ui| {
            self.install(ui, &k, profiles, outcome)
        });
        ui.add_space(14.0);
        self.actions(ui, &k, keys, profiles);
    }

    fn empty_state(&mut self, ui: &mut Ui, keys: &mut KeyStore, outcome: &mut KeyOutcome) {
        ui.add_space(70.0);
        ui.vertical_centered(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(64.0), Sense::hover());
            ui.painter().rect_filled(r, egui::CornerRadius::same(18), theme::accent().gamma_multiply(0.14));
            w::paint_icon(ui.painter(), r.shrink(18.0), Icon::Key, theme::accent());
            ui.add_space(14.0);
            let title = if keys.keys.is_empty() { "No SSH keys yet" } else { "Select a key" };
            ui.label(RichText::new(title).size(18.0).strong());
            ui.label(
                RichText::new("Keys log you in without typing a password. Generate one here, or import an existing ~/.ssh key.")
                    .size(12.5)
                    .color(theme::text_dim()),
            );
            ui.add_space(16.0);
        });
        let width = 300.0;
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
            ui.spacing_mut().item_spacing.x = 10.0;
            if form::button(ui, "Generate a key", Kind::Primary, self.pending.is_none()).clicked() {
                self.start_generate_form();
            }
            if form::button(ui, "Import a key file…", Kind::Secondary, true).clicked() {
                self.import(keys, outcome);
            }
        });
    }

    fn install(&mut self, ui: &mut Ui, k: &KeyMeta, profiles: &ProfileStore, outcome: &mut KeyOutcome) {
        if profiles.profiles.is_empty() {
            form::note(ui, "Add a host first.", false);
            return;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let chosen = self.install_host.and_then(|id| profiles.get(id)).map(|p| p.display_name().to_string()).unwrap_or_else(|| "Choose a host…".into());
            let width = (ui.available_width() - 120.0).clamp(160.0, 360.0);
            let mut pick: Option<Uuid> = None;
            let current = self.install_host;
            form::select(ui, egui::Id::new("key_install_host"), width, &chosen, |ui| {
                for p in &profiles.profiles {
                    if form::option(ui, p.display_name(), Some(&p.address()), current == Some(p.id)).clicked() {
                        pick = Some(p.id);
                    }
                }
            });
            if pick.is_some() {
                self.install_host = pick;
            }
            if form::button(ui, "Install key", Kind::Secondary, self.install_host.is_some()).clicked()
                && let Some(host) = self.install_host
            {
                *outcome = KeyOutcome::Install { key: k.id, host };
                self.message = Some((true, "Installing…".into()));
            }
        });
    }

    fn actions(&mut self, ui: &mut Ui, k: &KeyMeta, keys: &mut KeyStore, profiles: &ProfileStore) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            if form::button(ui, "Edit name / comment", Kind::Secondary, true).clicked() {
                self.form = Form::Edit { name: k.name.clone(), comment: k.comment.clone(), pass: String::new() };
            }
            let pass_label = if k.encrypted { "Change passphrase" } else { "Add passphrase" };
            if form::button(ui, pass_label, Kind::Secondary, true).clicked() {
                self.form = Form::Passphrase { old: String::new(), new: String::new(), new2: String::new(), remember: secret::get(k.id).is_some() };
            }
            let export_tip = if k.encrypted {
                "Copy the private key file (stays encrypted with its passphrase)"
            } else {
                "Copy the private key file. It has NO passphrase: anyone with the copy can log in as you"
            };
            if form::button(ui, "Export private…", Kind::Secondary, true).on_hover_text(export_tip).clicked()
                && let Some(path) = rfd::FileDialog::new().set_file_name(file_stem(&k.name)).save_file()
            {
                self.message = Some(match keys.export_private(k.id, &path) {
                    Ok(()) if k.encrypted => (true, format!("Exported to {}", path.display())),
                    Ok(()) => (true, format!("Exported to {}. It's unencrypted: keep that file private.", path.display())),
                    Err(e) => (false, format!("{e:#}")),
                });
            }
            if form::button(ui, "Delete", Kind::Ghost, true).clicked() {
                self.form = Form::Delete;
            }
        });
        ui.add_space(12.0);
        if self.pending.is_some() && matches!(self.form, Form::Edit { .. } | Form::Passphrase { .. }) {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
                loading::mini_spinner(ui, r.center(), 7.0, theme::accent());
                ui.label(RichText::new("Re-encrypting the key…").color(theme::text_dim()));
            });
            return;
        }
        let (done, msg, reselect, job) = match &mut self.form {
            Form::None => (false, None, false, None),
            Form::Edit { name, comment, pass } => edit_form(ui, k, keys, name, comment, pass, &mut self.show_pass),
            Form::Passphrase { old, new, new2, remember } => passphrase_form(ui, k, keys, old, new, new2, remember, &mut self.show_pass),
            Form::Delete => delete_form(ui, k, keys, profiles),
        };
        if let Some(m) = msg {
            self.message = Some(m);
        }
        if reselect {
            self.selected = keys.keys.first().map(|k| k.id);
            if self.selected.is_none() {
                self.panel = Panel::Details;
            }
        }
        if job.is_some() {
            self.pending = job;
            self.message = None;
        }
        if done {
            self.form = Form::None;
        }
    }
}

/// (form finished, message, select another key, worker job)
type FormResult = (bool, Option<(bool, String)>, bool, Option<super::Job>);

fn fingerprint(ui: &mut Ui, k: &KeyMeta, profiles: &ProfileStore) {
    ui.horizontal_top(|ui| {
        if let Some(art) = k.randomart() {
            egui::Frame::new().fill(form::input_fill()).stroke(egui::Stroke::new(1.0, theme::border())).corner_radius(8).inner_margin(10).show(ui, |ui| {
                ui.label(RichText::new(art).monospace().size(10.5).color(theme::text_dim()));
            });
            ui.add_space(8.0);
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.add(egui::Label::new(RichText::new(&k.fingerprint).monospace().size(12.5)).wrap());
            if form::button(ui, "Copy fingerprint", Kind::Secondary, true).clicked() {
                ui.ctx().copy_text(k.fingerprint.clone());
            }
            let users: Vec<&str> = profiles.profiles.iter().filter(|p| uses_key(p, k.id)).map(|p| p.display_name()).collect();
            let used = if users.is_empty() { "Not used by any host yet".to_string() } else { format!("Used by {}", users.join(", ")) };
            form::note(ui, &used, false);
        });
    });
}

fn edit_form(ui: &mut Ui, k: &KeyMeta, keys: &mut KeyStore, name: &mut String, comment: &mut String, pass: &mut String, show: &mut bool) -> FormResult {
    let mut out: FormResult = (false, None, false, None);
    form::section(ui, None, "Edit key", "The name is only shown here; the comment is stored in the key", |ui| {
        let width = ui.available_width();
        form::field(ui, width, "Name", None, None, |ui| form::text(ui, name, "Laptop 2026", width, false));
        ui.add_space(8.0);
        form::field(ui, width, "Comment", None, None, |ui| form::text(ui, comment, "you@pc", width, false));
        if k.encrypted && *comment != k.comment {
            ui.add_space(8.0);
            form::field(ui, width, "Passphrase", None, Some("Needed to rewrite the comment"), |ui| form::secret(ui, pass, "", show, width, false));
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if form::button(ui, "Save", Kind::Primary, !name.trim().is_empty()).clicked() {
                let pass = Some(pass.as_str()).filter(|p| !p.is_empty()).map(str::to_string).or_else(|| secret::get(k.id));
                match keys.prepare_edit(k.id, comment, pass) {
                    // Only the label changed: instant.
                    Ok(None) => {
                        keys.rename(k.id, name);
                        out.1 = Some(match keys.save() {
                            Ok(()) => (true, "Key updated".into()),
                            Err(e) => (false, format!("{e:#}")),
                        });
                        out.0 = true;
                    }
                    Ok(Some(work)) => {
                        let (id, name) = (k.id, name.clone());
                        out.3 = Some(spawn(move || {
                            let rewrite = work()?;
                            Ok(Done::Rewritten { rewrite, id, name: Some(name), remember: Remember::Keep, note: "Key updated".into() })
                        }));
                        out.0 = true;
                    }
                    Err(e) => out.1 = Some((false, format!("{e:#}"))),
                }
            }
            if form::button(ui, "Cancel", Kind::Ghost, true).clicked() {
                out.0 = true;
            }
        });
    });
    out
}

#[allow(clippy::too_many_arguments)]
fn passphrase_form(
    ui: &mut Ui,
    k: &KeyMeta,
    keys: &KeyStore,
    old: &mut String,
    new: &mut String,
    new2: &mut String,
    remember: &mut bool,
    show: &mut bool,
) -> FormResult {
    let mut out: FormResult = (false, None, false, None);
    let title = if k.encrypted { "Change passphrase" } else { "Add passphrase" };
    form::section(ui, None, title, "The key file is re-encrypted; servers don't need to change", |ui| {
        let width = ui.available_width();
        if k.encrypted {
            form::field(ui, width, "Current passphrase", None, None, |ui| form::secret(ui, old, "", show, width, false));
            ui.add_space(8.0);
        }
        let half = (width - form::GAP) / 2.0;
        let mismatch = new != new2;
        form::row2(ui, half, |ui, lw, rw| {
            form::field(ui, lw, "New passphrase", None, Some("Leave empty to remove it"), |ui| form::secret(ui, new, "", show, lw, false));
            let err = (mismatch && !new2.is_empty()).then_some("Passphrases don't match");
            form::field(ui, rw, "Repeat", err, None, |ui| form::secret(ui, new2, "", show, rw, err.is_some()));
        });
        super::generate::strength(ui, new);
        ui.add_space(4.0);
        ui.checkbox(remember, format!("Remember in {}", crate::platform::credential_store()));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if form::button(ui, "Apply", Kind::Primary, !mismatch).clicked() {
                let old_pass = Some(old.as_str()).filter(|p| !p.is_empty()).map(str::to_string).or_else(|| secret::get(k.id));
                let (id, path, new_pass) = (k.id, keys.key_file(k.id), new.clone());
                let remember = match (*remember, new_pass.is_empty()) {
                    (true, false) => Remember::Store(new_pass.clone()),
                    _ => Remember::Forget,
                };
                let note = if new_pass.is_empty() { "Passphrase removed" } else { "Passphrase changed" }.to_string();
                out.3 = Some(spawn(move || {
                    let rewrite = keystore::prepare_rewrite(&path, id, old_pass.as_deref(), Some(&new_pass), None)?;
                    Ok(Done::Rewritten { rewrite, id, name: None, remember, note })
                }));
                out.0 = true;
            }
            if form::button(ui, "Cancel", Kind::Ghost, true).clicked() {
                out.0 = true;
            }
        });
    });
    out
}

fn delete_form(ui: &mut Ui, k: &KeyMeta, keys: &mut KeyStore, profiles: &ProfileStore) -> FormResult {
    let mut out: FormResult = (false, None, false, None);
    let users = profiles.profiles.iter().filter(|p| uses_key(p, k.id)).count();
    let warn = if users > 0 {
        format!("{users} host{} use this key and will stop authenticating with it. This can't be undone.", if users == 1 { "" } else { "s" })
    } else {
        "The private key file is deleted. This can't be undone.".into()
    };
    form::section(ui, None, &format!("Delete “{}”?", k.name), &warn, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if form::button(ui, "Delete for good", Kind::Danger, true).clicked() {
                match keys.remove(k.id) {
                    Ok(()) => {
                        out.1 = Some((true, format!("Deleted “{}”", k.name)));
                        out.2 = true;
                    }
                    Err(e) => out.1 = Some((false, format!("{e:#}"))),
                }
                out.0 = true;
            }
            if form::button(ui, "Cancel", Kind::Ghost, true).clicked() {
                out.0 = true;
            }
        });
    });
    out
}
