//! Modal dialogs of the SFTP view (new folder, rename, delete, editor refusal). A child module of
//! `sftp_view` so it can drive the view's private state.

use std::path::PathBuf;

use eframe::egui::{self, Color32};

use super::SftpView;
use crate::ssh::sftp;
use crate::ssh::transfer::Direction;
use crate::theme;
use crate::ui::file_list::Side;

#[derive(Debug, Clone)]
pub(super) struct TransferItem {
    pub local: PathBuf,
    pub remote: String,
    pub name: String,
}

pub(super) enum Dialog {
    NewFolder {
        side: Side,
        name: String,
    },
    Rename {
        side: Side,
        old: String,
        new: String,
        /// The name stem is pre-selected on the first frame, like Explorer.
        selected: bool,
    },
    Delete {
        side: Side,
        names: Vec<String>,
    },
    /// Confirmation before overwriting existing file(s) during upload or download.
    Replace {
        direction: Direction,
        items: Vec<TransferItem>,
        conflicts: Vec<String>,
    },
    /// The editor refused a remote file; offer the system app instead.
    OpenRefused {
        name: String,
        reason: String,
    },
}

impl Dialog {
    /// False while a name field is empty or unchanged, so Enter can't submit nothing.
    fn is_ready(&self) -> bool {
        match self {
            Dialog::NewFolder { name, .. } => !name.trim().is_empty(),
            Dialog::Rename { old, new, .. } => !new.trim().is_empty() && new.trim() != old,
            _ => true,
        }
    }
}

/// Selects the name without its extension (the whole name for folders and dotfiles).
fn select_stem(ui: &egui::Ui, out: &egui::text_edit::TextEditOutput, name: &str) {
    let end = name.rfind('.').filter(|&i| i > 0).unwrap_or(name.len());
    let end = name[..end].chars().count();
    let mut state = out.state.clone();
    state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(end))));
    state.store(ui.ctx(), out.response.id);
}

impl SftpView {
    pub(super) fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.dialog.take() else { return };
        let mut keep = true;
        let mut confirmed = false;
        let mut skip_existing = false;
        let mut dialog = dialog;
        // Enter confirms every dialog (Esc closes it, handled by the modal). Read before
        // the text field, which surrenders focus on Enter.
        let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
        let modal = egui::Modal::new(egui::Id::new("sftp_dialog")).show(ctx, |ui| {
            let width = if matches!(dialog, Dialog::Replace { .. }) { 360.0 } else { 320.0 };
            ui.set_width(width);
            match &mut dialog {
                Dialog::NewFolder { name, .. } => {
                    ui.heading("New folder");
                    let r = ui.add(egui::TextEdit::singleline(name).hint_text("folder name").desired_width(f32::INFINITY));
                    r.request_focus();
                }
                Dialog::Rename { new, selected, .. } => {
                    ui.heading("Rename");
                    let out = egui::TextEdit::singleline(new).desired_width(f32::INFINITY).show(ui);
                    out.response.request_focus();
                    if !std::mem::replace(selected, true) {
                        select_stem(ui, &out, new);
                    }
                }
                Dialog::Delete { names, .. } => {
                    ui.heading("Delete permanently?");
                    let preview = names.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
                    let more = names.len().saturating_sub(5);
                    ui.label(if more > 0 { format!("{preview} and {more} more") } else { preview });
                    ui.colored_label(theme::danger(), "Folders are deleted recursively. This cannot be undone.");
                }
                Dialog::Replace { direction, items, conflicts } => {
                    let target_side = match direction {
                        Direction::Upload => "remote",
                        Direction::Download => "local",
                    };
                    if conflicts.len() == 1 {
                        ui.heading(format!("Replace “{}”?", conflicts[0]));
                        ui.label(
                            egui::RichText::new(format!("“{}” already exists in the {target_side} folder. Are you sure you want to replace it?", conflicts[0]))
                                .color(theme::text_dim()),
                        );
                    } else {
                        ui.heading(format!("Replace {} existing files?", conflicts.len()));
                        let summary = if items.len() > conflicts.len() {
                            format!("{} of the {} selected items already exist in the {target_side} folder and will be replaced:", conflicts.len(), items.len())
                        } else {
                            format!("The following {} items already exist in the {target_side} folder and will be replaced:", conflicts.len())
                        };
                        ui.label(egui::RichText::new(summary).color(theme::text_dim()));
                        ui.add_space(6.0);
                        egui::Frame::new().fill(theme::surface_2()).corner_radius(egui::CornerRadius::same(6)).inner_margin(8.0).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            egui::ScrollArea::vertical().max_height(160.0).auto_shrink([false, true]).show(ui, |ui| {
                                for name in conflicts.iter() {
                                    ui.label(egui::RichText::new(format!("• {name}")).monospace().size(12.0).color(theme::text()));
                                }
                            });
                        });
                    }
                }
                Dialog::OpenRefused { name, reason } => {
                    ui.heading(format!("Can't open {name} in the editor"));
                    ui.label(egui::RichText::new(reason.as_str()).color(theme::text_dim()));
                    ui.label("Open it with the default app on this PC instead? Saved changes are uploaded automatically.");
                }
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let label = match &dialog {
                    Dialog::Delete { .. } => "Delete",
                    Dialog::Replace { conflicts, .. } => {
                        if conflicts.len() > 1 {
                            "Replace all"
                        } else {
                            "Replace"
                        }
                    }
                    Dialog::OpenRefused { .. } => "Open with system app",
                    Dialog::Rename { .. } => "Rename",
                    Dialog::NewFolder { .. } => "Create",
                };
                let color = if matches!(dialog, Dialog::Delete { .. } | Dialog::Replace { .. }) { theme::danger() } else { theme::accent() };
                confirmed |= ui.add(egui::Button::new(egui::RichText::new(label).color(Color32::WHITE)).fill(color.gamma_multiply(0.75))).clicked();
                if let Dialog::Replace { items, conflicts, .. } = &dialog
                    && items.len() > conflicts.len()
                    && ui.button("Skip existing").clicked()
                {
                    skip_existing = true;
                }
                if ui.button("Cancel").clicked() {
                    keep = false;
                }
                ui.label(egui::RichText::new("Enter to confirm · Esc to cancel").size(11.0).color(theme::text_faint()));
            });
        });
        if modal.should_close() {
            keep = false;
        }
        confirmed |= enter && keep && dialog.is_ready();
        if skip_existing {
            if let Dialog::Replace { direction, items, conflicts } = dialog {
                let non_conflicting: Vec<TransferItem> = items.into_iter().filter(|it| !conflicts.contains(&it.name)).collect();
                self.spawn_transfers(direction, non_conflicting);
            }
        } else if confirmed {
            self.run_dialog(dialog);
        } else if keep {
            self.dialog = Some(dialog);
        }
    }

    fn run_dialog(&mut self, dialog: Dialog) {
        let rcwd = self.remote_cwd();
        match dialog {
            Dialog::NewFolder { side: Side::Local, name } => {
                let p = self.local_cwd.join(name.trim());
                self.local_op(|| std::fs::create_dir(p));
            }
            Dialog::NewFolder { side: Side::Remote, name } => {
                let p = sftp::join(&rcwd, name.trim());
                self.remote_op(move |s| async move { s.mkdir(&p).await });
            }
            Dialog::Rename { side: Side::Local, old, new, .. } => {
                let (a, b) = (self.local_cwd.join(old), self.local_cwd.join(new.trim()));
                // std::fs::rename replaces an existing target on Windows; never do that silently.
                self.local_op(|| {
                    if b.exists() {
                        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} already exists", b.display())));
                    }
                    std::fs::rename(a, b)
                });
            }
            Dialog::Rename { side: Side::Remote, old, new, .. } => {
                let (a, b) = (sftp::join(&rcwd, &old), sftp::join(&rcwd, new.trim()));
                self.remote_op(move |s| async move { s.rename(&a, &b).await });
            }
            Dialog::Delete { side: Side::Local, names } => {
                let paths: Vec<PathBuf> = names.iter().map(|n| self.local_cwd.join(n)).collect();
                self.local_op(|| {
                    for p in paths {
                        if p.is_dir() { std::fs::remove_dir_all(p)? } else { std::fs::remove_file(p)? }
                    }
                    Ok(())
                });
                self.local_list.clear();
            }
            Dialog::Delete { side: Side::Remote, names } => {
                let rows = self.remote.lock().map(|s| s.rows.clone()).unwrap_or_default();
                let targets: Vec<(String, bool)> =
                    names.iter().map(|n| (sftp::join(&rcwd, n), rows.iter().any(|r| &r.name == n && r.is_dir && !r.is_link))).collect();
                self.remote_list.clear();
                self.remote_op(move |s| async move {
                    for (path, is_dir) in targets {
                        s.remove(&path, is_dir).await?;
                    }
                    Ok(())
                });
            }
            Dialog::Replace { direction, items, .. } => {
                self.spawn_transfers(direction, items);
            }
            Dialog::OpenRefused { name, .. } => self.open_with_system(name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_only_submits_a_real_name() {
        let rename = |new: &str| Dialog::Rename { side: Side::Remote, old: "a.txt".into(), new: new.into(), selected: true };
        assert!(!rename("a.txt").is_ready());
        assert!(!rename("  ").is_ready());
        assert!(rename("b.txt").is_ready());
        assert!(!Dialog::NewFolder { side: Side::Local, name: " ".into() }.is_ready());
        assert!(Dialog::Delete { side: Side::Local, names: vec!["x".into()] }.is_ready());
        assert!(Dialog::Replace { direction: Direction::Upload, items: Vec::new(), conflicts: vec!["x.txt".into()] }.is_ready());
    }
}
