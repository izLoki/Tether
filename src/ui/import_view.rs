//! "Import hosts" modal: auto-detected Bitvise profiles and SSH config hosts, plus
//! file pickers for profiles stored elsewhere.

use std::path::Path;

use eframe::egui::{self, RichText, Ui};

use super::form::{self, Kind};
use super::settings_widgets::Icon;
use crate::import::{Candidate, Source, bitvise, openssh};
use crate::profile::{Profile, ProfileStore};
use crate::theme;

struct Row {
    candidate: Candidate,
    selected: bool,
    exists: bool,
}

pub struct ImportDialog {
    rows: Vec<Row>,
    notice: Option<String>,
    export: bool,
    acknowledged: bool,
    scan: Option<std::sync::mpsc::Receiver<crate::import::discovery::Found>>,
    searched: Vec<std::path::PathBuf>,
}

pub enum ImportOutcome {
    Open,
    Cancel,
    Import(Vec<Profile>),
}

impl ImportDialog {
    pub fn new(store: &ProfileStore) -> Self {
        let mut dialog = Self { rows: Vec::new(), notice: None, export: false, acknowledged: false, scan: None, searched: Vec::new() };
        dialog.add(bitvise::discover(), store);
        if let Some(path) = openssh::default_path() {
            dialog.add(openssh::load(&path), store);
        }
        dialog
    }

    fn add(&mut self, candidates: Vec<Candidate>, store: &ProfileStore) -> usize {
        let mut added = 0;
        for c in candidates {
            let duplicate = self.rows.iter().any(|r| r.candidate.origin == c.origin && r.candidate.profile.name == c.profile.name);
            if duplicate {
                continue;
            }
            let exists = c.exists_in(store)
                || self.rows.iter().any(|r| {
                    let p = &r.candidate.profile;
                    p.host.eq_ignore_ascii_case(&c.profile.host) && p.port == c.profile.port && p.user == c.profile.user
                });
            self.rows.push(Row { candidate: c, selected: !exists, exists });
            added += 1;
        }
        added
    }

    fn pick_bitvise(&mut self, store: &ProfileStore) {
        let start = bitvise::search_dirs().into_iter().find(|d| d.is_dir()).unwrap_or_default();
        let Some(files) = rfd::FileDialog::new()
            .set_title("Choose Bitvise profiles")
            .set_directory(start)
            .add_filter("Bitvise profile", &["tlp"])
            .add_filter("All files", &["*"])
            .pick_files()
        else {
            return;
        };
        let found: Vec<Candidate> = files.iter().filter_map(|f| bitvise::load(f)).collect();
        let skipped = files.len() - found.len();
        let added = self.add(found, store);
        self.notice = Some(match skipped {
            0 => format!("{added} profile(s) added"),
            n => format!("{added} profile(s) added · {n} file(s) were not Bitvise profiles"),
        });
    }

    fn pick_ssh_config(&mut self, store: &ProfileStore) {
        let Some(file) = rfd::FileDialog::new().set_title("Choose an SSH config file").pick_file() else { return };
        let added = self.add(openssh::load(&file), store);
        self.notice = Some(format!("{added} host(s) added from {}", file.display()));
    }

    pub fn show(&mut self, ctx: &egui::Context, store: &ProfileStore) -> ImportOutcome {
        if let Some(result) = self.scan.as_ref().map(|rx| rx.try_recv()) {
            match result {
                Ok(found) => {
                    self.searched = found.searched;
                    let n = self.add(found.profiles, store);
                    self.notice = Some(format!(
                        "Found {n} Bitvise profile(s){}",
                        if found.limited { " · Search limit reached; use Browse for other folders" } else { "" }
                    ));
                    self.scan = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.notice = Some("Search stopped. You can still browse for files manually.".into());
                    self.scan = None;
                }
                Err(_) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
        let mut outcome = ImportOutcome::Open;
        let modal = egui::Modal::new(egui::Id::new("import_dialog")).frame(form::modal_frame()).show(ctx, |ui| {
            egui::Frame::new().inner_margin(24).show(ui, |ui| {
                ui.set_width(640.0_f32.min(ctx.content_rect().width() - 80.0));
                ui.horizontal(|ui| {
                    form::icon_tile(ui, Icon::Server, theme::accent());
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Import / export machines").size(19.0).strong());
                        ui.label(RichText::new("Move your connection setup between devices.").size(12.0).color(theme::text_dim()));
                    });
                });
                ui.add_space(20.0);
                form::segmented(ui, &mut self.export, &[(false, "Import", "Add saved connections"), (true, "Export setup", "Take your machines with you")]);
                ui.add_space(16.0);
                egui::ScrollArea::vertical()
                    .id_salt(("setup_body", self.export))
                    .max_height((ctx.content_rect().height() - 310.0).clamp(160.0, 480.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if self.export {
                            self.export_ui(ui, store);
                        } else {
                            self.import_ui(ui, store);
                        }
                    });
                if let Some(notice) = &self.notice {
                    ui.add_space(10.0);
                    form::note(ui, notice, false);
                }
                ui.add_space(16.0);
                form::rule(ui);
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let count = if self.export { store.profiles.len() } else { self.rows.iter().filter(|r| r.selected && !r.exists).count() };
                    let noun = if count == 1 { "machine" } else { "machines" };
                    ui.label(
                        RichText::new(format!("{count} {noun} {}", if self.export { "in this setup" } else { "selected" })).size(12.0).color(theme::text_dim()),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let enabled = count > 0 && (!self.export || self.acknowledged);
                        let label = if self.export { format!("Export {count} {noun}…") } else { format!("Import {count}") };
                        if form::button(ui, &label, Kind::Primary, enabled).clicked() {
                            if self.export {
                                self.save_export(store);
                            } else {
                                outcome =
                                    ImportOutcome::Import(self.rows.iter().filter(|r| r.selected && !r.exists).map(|r| r.candidate.profile.clone()).collect());
                            }
                        }
                        if form::button(ui, if self.export { "Close" } else { "Cancel" }, Kind::Secondary, true).clicked() {
                            outcome = ImportOutcome::Cancel;
                        }
                    });
                });
            });
        });
        if modal.should_close() && matches!(outcome, ImportOutcome::Open) {
            outcome = ImportOutcome::Cancel;
        }
        outcome
    }

    fn import_ui(&mut self, ui: &mut Ui, store: &ProfileStore) {
        form::section(ui, Some(Icon::Folder), "Choose a source", "Bring in a Tether setup or connections from another SSH client.", |ui| {
            ui.columns(3, |columns| {
                if source_button(&mut columns[0], Icon::Server, "Tether setup", "Portable .json file").clicked()
                    && let Some(path) = rfd::FileDialog::new().add_filter("Tether setup", &["json"]).pick_file()
                {
                    self.notice = Some(match crate::import::setup::load(&path) {
                        Ok(candidates) => {
                            let n = self.add(candidates, store);
                            format!("{n} host(s) ready to import")
                        }
                        Err(e) => format!("Could not import: {e:#}"),
                    });
                }
                if source_button(&mut columns[1], Icon::Terminal, "Bitvise profiles", "Browse .tlp files").clicked() {
                    self.pick_bitvise(store);
                }
                if source_button(&mut columns[2], Icon::FileType, "OpenSSH config", "Choose a config file").clicked() {
                    self.pick_ssh_config(store);
                }
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if form::button(ui, "Search this PC", Kind::Secondary, self.scan.is_none()).clicked() {
                    let (tx, rx) = std::sync::mpsc::channel();
                    self.scan = Some(rx);
                    let ctx = ui.ctx().clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(crate::import::discovery::scan(crate::import::discovery::roots()));
                        ctx.request_repaint();
                    });
                }
                if self.scan.is_some() {
                    ui.spinner();
                }
                ui.label(RichText::new("Find Bitvise profiles in common locations").size(11.5).color(theme::text_faint()));
            });
            if !self.searched.is_empty() {
                ui.collapsing("Searched locations", |ui| {
                    for path in &self.searched {
                        ui.add(egui::Label::new(RichText::new(path.display().to_string()).size(11.0).color(theme::text_dim())).wrap());
                    }
                });
            }
        });
        ui.add_space(12.0);
        form::section(ui, Some(Icon::Rows), "Choose machines", "Import selected connections. Saved connections are skipped.", |ui| self.list(ui));
    }

    fn export_ui(&mut self, ui: &mut Ui, store: &ProfileStore) {
        form::section(ui, Some(Icon::Server), "Your machines", "One portable setup file, ready to import on another device.", |ui| {
            if store.profiles.is_empty() {
                form::note(ui, "Add a machine before exporting a setup.", false);
            }
            egui::ScrollArea::vertical().id_salt("export_machines").max_height(135.0).show(ui, |ui| {
                for p in &store.profiles {
                    machine_row(ui, p, None);
                }
            });
        });
        ui.add_space(12.0);
        form::section(ui, Some(Icon::Key), "Connection details only", "Credentials stay on this device.", |ui| {
            ui.columns(2, |columns| {
                columns[0].label(RichText::new("INCLUDED").size(10.5).strong().color(theme::accent()));
                columns[0].label(
                    RichText::new(
                        "Names, IPs / hostnames, ports,
usernames and colors",
                    )
                    .size(12.5)
                    .color(theme::text_dim()),
                );
                columns[1].label(RichText::new("EXCLUDED").size(10.5).strong().color(theme::text_faint()));
                columns[1].label(
                    RichText::new(
                        "SSH keys, key paths, passwords
and passphrases",
                    )
                    .size(12.5)
                    .color(theme::text_dim()),
                );
            });
            ui.add_space(12.0);
            form::rule(ui);
            ui.add_space(10.0);
            ui.label(
                RichText::new("Anyone with this file can import these connection details. Credentials are still required to connect.")
                    .size(12.0)
                    .color(theme::text_dim()),
            );
            ui.add_space(8.0);
            ui.scope(|ui| {
                let red = theme::danger();
                let fill = form::input_fill().lerp_to_gamma(red, 0.2);
                let w = &mut ui.visuals_mut().widgets;
                for state in [&mut w.inactive, &mut w.hovered, &mut w.active] {
                    state.bg_fill = fill;
                    state.weak_bg_fill = fill;
                    state.bg_stroke = egui::Stroke::new(1.0, red.gamma_multiply(0.75));
                    state.fg_stroke = egui::Stroke::new(2.0, red);
                    state.corner_radius = egui::CornerRadius::same(3);
                }
                ui.checkbox(&mut self.acknowledged, "I understand the file contains connection details");
            });
        });
    }

    fn save_export(&mut self, store: &ProfileStore) {
        if let Some(path) = rfd::FileDialog::new().set_file_name("tether-setup.json").add_filter("Tether setup", &["json"]).save_file() {
            let result = crate::import::setup::encode(&store.profiles).and_then(|text| std::fs::write(&path, text).map_err(Into::into));
            self.notice = Some(match result {
                Ok(()) => format!("Exported to {}", path.display()),
                Err(e) => format!("Export failed: {e:#}"),
            });
        }
    }

    fn list(&mut self, ui: &mut Ui) {
        if self.rows.is_empty() {
            ui.add_space(10.0);
            ui.vertical_centered(|ui| {
                form::icon_tile(ui, Icon::Server, theme::text_faint());
                ui.add_space(8.0);
                ui.label(RichText::new("Ready to bring your machines over").size(13.0).strong());
                ui.label(RichText::new("Choose a file above or search this PC to get started.").size(12.0).color(theme::text_dim()));
            });
            ui.add_space(12.0);
            return;
        }
        ui.horizontal(|ui| {
            if ui.small_button("Select new").clicked() {
                self.rows.iter_mut().for_each(|r| r.selected = !r.exists);
            }
            if ui.small_button("Select none").clicked() {
                self.rows.iter_mut().for_each(|r| r.selected = false);
            }
        });
        ui.add_space(6.0);
        for source in [Source::Tether, Source::Bitvise, Source::OpenSsh] {
            if !self.rows.iter().any(|r| r.candidate.source == source) {
                continue;
            }
            ui.add_space(4.0);
            ui.label(RichText::new(source.label().to_uppercase()).size(10.5).strong().color(theme::text_faint()));
            for row in self.rows.iter_mut().filter(|r| r.candidate.source == source) {
                egui::Frame::new().fill(form::input_fill()).corner_radius(8).inner_margin(10).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.add_enabled(!row.exists, egui::Checkbox::new(&mut row.selected, ""));
                        ui.vertical(|ui| {
                            machine_row(ui, &row.candidate.profile, row.exists.then_some("Already saved"));
                            ui.add(
                                egui::Label::new(RichText::new(row.candidate.origin.display().to_string()).size(10.5).color(theme::text_faint())).truncate(),
                            )
                            .on_hover_text(origin_label(&row.candidate.origin));
                        });
                    });
                });
                ui.add_space(4.0);
            }
        }
    }
}

fn source_button(ui: &mut Ui, icon: Icon, title: &str, hint: &str) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 78.0), egui::Sense::click());
    let fill = if response.hovered() { theme::accent_soft() } else { form::input_fill() };
    ui.painter().rect(rect, 9, fill, egui::Stroke::new(1.0, theme::border()), egui::StrokeKind::Inside);
    super::settings_widgets::paint_icon(
        ui.painter(),
        egui::Rect::from_min_size(rect.min + egui::vec2(12.0, 12.0), egui::vec2(18.0, 18.0)),
        icon,
        theme::accent(),
    );
    ui.painter().text(rect.min + egui::vec2(12.0, 39.0), egui::Align2::LEFT_TOP, title, egui::FontId::proportional(12.5), theme::text());
    ui.painter().text(rect.min + egui::vec2(12.0, 57.0), egui::Align2::LEFT_TOP, hint, egui::FontId::proportional(10.5), theme::text_faint());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, title));
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn machine_row(ui: &mut Ui, p: &Profile, badge: Option<&str>) {
    ui.horizontal(|ui| {
        let (dot, _) = ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
        ui.painter().circle_filled(dot.center(), 3.0, theme::hue_color(p.hue));
        ui.label(RichText::new(p.display_name()).size(12.5).strong());
        if let Some(badge) = badge {
            ui.label(RichText::new(badge).size(10.5).color(theme::text_faint()));
        }
    });
    ui.label(RichText::new(p.address()).size(11.5).color(theme::text_dim()));
    ui.add_space(5.0);
}

fn origin_label(p: &Path) -> String {
    format!("From {}", p.display())
}
