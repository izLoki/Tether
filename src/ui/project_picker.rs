//! A directory field with the same completion engine as SFTP, plus local shortcuts.
use super::{
    form,
    path_complete::{Candidate, LOCAL},
    path_field::{PathEvent, PathField},
};
use crate::{local_projects as projects, settings, theme};
use eframe::egui::{self, RichText};
use std::sync::mpsc::{self, Receiver};

type ListingResult = (String, Result<Vec<Candidate>, String>);

pub struct ProjectPicker {
    field: PathField,
    listing: Option<(String, Receiver<ListingResult>)>,
    next_listing: Option<String>,
    project_name: String,
    error: Option<String>,
}

impl Default for ProjectPicker {
    fn default() -> Self {
        let mut field = PathField::new(egui::Id::new("agent_project_path"), LOCAL);
        field.retain_edits = true;
        let last = settings::snapshot().projects.last_directory.clone();
        field.text = if last.is_empty() { projects::home().display().to_string() } else { last };
        Self { field, listing: None, next_listing: None, project_name: String::new(), error: None }
    }
}

impl ProjectPicker {
    fn request_listing(&mut self, dir: String, ctx: &egui::Context) {
        if let Some((current, _)) = &self.listing {
            if current != &dir {
                self.next_listing = Some(dir);
            }
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.listing = Some((dir.clone(), rx));
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = std::fs::read_dir(projects::resolve(&dir))
                .map(|entries| {
                    entries
                        .take(4096)
                        .filter_map(Result::ok)
                        .filter(|e| e.path().is_dir())
                        .map(|e| Candidate { name: e.file_name().to_string_lossy().into_owned(), is_dir: true })
                        .collect()
                })
                .map_err(|e| e.to_string());
            let _ = tx.send((dir, result));
            ctx.request_repaint();
        });
    }
    pub fn directory(&mut self) -> Option<std::path::PathBuf> {
        let dir = projects::resolve(&self.field.text);
        if !self.field.text.trim().is_empty() && dir.is_dir() {
            self.error = None;
            Some(dir)
        } else {
            self.error = Some("Choose an existing local folder before starting.".into());
            None
        }
    }

    pub fn focus(&self, ctx: &egui::Context) {
        ctx.memory_mut(|m| m.request_focus(self.field.id()));
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        if let Some((_, rx)) = &self.listing {
            match rx.try_recv() {
                Ok((dir, result)) => {
                    match result {
                        Ok(entries) => self.field.provide_listing(dir, entries),
                        Err(_) => self.field.listing_failed(&dir),
                    }
                    self.listing = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.listing = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.listing.is_none()
            && let Some(dir) = self.next_listing.take()
        {
            self.request_listing(dir, ui.ctx());
        }
        form::section(
            ui,
            Some(super::settings_widgets::Icon::Folder),
            "Project directory",
            "Tab completes folders · Shift+Tab cycles backwards · ~ opens home",
            |ui| {
                let home = projects::home().display().to_string();
                if ui.memory(|m| m.has_focus(self.field.id())) && ui.input(|i| i.key_pressed(egui::Key::Tab)) && self.field.text.starts_with('~') {
                    self.field.text = projects::resolve(&self.field.text).display().to_string();
                }
                let width = ui.available_width();
                let events = self.field.show(ui, &home, None, width, None);
                for event in events {
                    match event {
                        PathEvent::Navigate(path) => self.field.text = projects::resolve(&path).display().to_string(),
                        PathEvent::NeedListing(dir) | PathEvent::Prefetch(dir) => self.request_listing(dir, ui.ctx()),
                    }
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if form::button(ui, "Browse…", form::Kind::Secondary, true).clicked()
                        && let Some(dir) =
                            rfd::FileDialog::new().set_title("Project directory").set_directory(projects::resolve(&self.field.text)).pick_folder()
                    {
                        self.field.text = dir.display().to_string();
                        self.error = None;
                    }
                    ui.add(form::style_edit(egui::TextEdit::singleline(&mut self.project_name).hint_text("Project name (optional)"), 205.0));
                    if form::button(ui, "☆ Save", form::Kind::Secondary, true).clicked()
                        && let Some(dir) = self.directory()
                    {
                        let path = dir.display().to_string();
                        let name = self.project_name.trim().to_owned();
                        if let Err(e) = settings::update(|s| {
                            s.projects.record(&path, projects::now());
                            if let Some(p) = s.projects.entries.iter_mut().find(|p| projects::same_path(&p.path, &path)) {
                                p.saved = true;
                                if !name.is_empty() {
                                    p.name.clone_from(&name);
                                }
                            }
                        }) {
                            self.error = Some(format!("Could not save project: {e}"));
                        }
                    }
                });
                let prefs = settings::snapshot();
                if !prefs.projects.entries.is_empty() {
                    ui.add_space(10.0);
                    form::label(ui, "Saved projects & recent folders", false);
                    let mut entries: Vec<_> = prefs.projects.entries.iter().collect();
                    entries.sort_by_key(|p| (std::cmp::Reverse(p.saved), std::cmp::Reverse(p.last_visit)));
                    egui::ScrollArea::vertical().id_salt("boot_projects").max_height(116.0).show(ui, |ui| {
                        for p in entries {
                            ui.horizontal(|ui| {
                                let selected = projects::same_path(&self.field.text, &p.path);
                                let label = format!("{}  {}", if p.saved { "★" } else { "↳" }, p.name);
                                let response = ui.add(egui::Button::selectable(selected, label)).on_hover_text(&p.path);
                                if response.clicked() {
                                    self.field.text.clone_from(&p.path);
                                    self.project_name.clone_from(&p.name);
                                    self.error = None;
                                }
                                response.context_menu(|ui| {
                                    if ui.button(if p.saved { "Unpin project" } else { "Save project" }).clicked() {
                                        let _ = settings::update(|s| {
                                            if let Some(entry) = s.projects.entries.iter_mut().find(|e| e.path == p.path) {
                                                entry.saved = !p.saved;
                                            }
                                        });
                                        ui.close();
                                    }
                                    if ui.button("Forget folder").clicked() {
                                        let _ = settings::update(|s| s.projects.entries.retain(|e| e.path != p.path));
                                        ui.close();
                                    }
                                });
                                ui.add(egui::Label::new(RichText::new(&p.path).size(11.0).color(theme::text_dim())).truncate()).on_hover_text(&p.path);
                            });
                        }
                    });
                }
                if let Some(error) = &self.error {
                    form::note(ui, error, true);
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    #[test]
    fn tab_completes_a_real_folder_and_keeps_it_when_focus_moves() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("Project with spaces");
        std::fs::create_dir(&target).unwrap();
        let mut picker = ProjectPicker::default();
        picker.field.text = root.path().join("Pro").display().to_string();
        let actual = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let output = actual.clone();
        let mut initial = true;
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(700.0, 700.0)).build_ui(move |ui| {
            if initial {
                picker.focus(ui.ctx());
                initial = false;
            }
            picker.show(ui);
            *output.lock().unwrap() = picker.field.text.clone();
        });
        h.run_steps(3);
        h.key_press(egui::Key::Tab);
        for _ in 0..50 {
            h.run_steps(2);
            if actual.lock().unwrap().ends_with(std::path::MAIN_SEPARATOR) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let completed = actual.lock().unwrap().clone();
        assert!(projects::same_path(&completed, &target.display().to_string()), "{completed}");
        h.get_all_by_role(egui::accesskit::Role::TextInput).nth(1).unwrap().focus();
        h.run_steps(3);
        assert_eq!(*actual.lock().unwrap(), completed);
    }
}
