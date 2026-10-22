//! Explicitly pick a discovered key; discovery never imports or changes credentials.
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use crate::import::discovery::{self, Found};
use crate::theme;
use eframe::egui::{self, RichText};

pub struct KeySearch {
    rx: Receiver<Found>,
    found: Option<Found>,
    filter: String,
}

impl KeySearch {
    pub fn new(ctx: &egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(discovery::scan(discovery::roots()));
            ctx.request_repaint();
        });
        Self { rx, found: None, filter: String::new() }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> (Option<PathBuf>, bool) {
        if self.found.is_none() {
            match self.rx.try_recv() {
                Ok(found) => self.found = Some(found),
                Err(mpsc::TryRecvError::Disconnected) => self.found = Some(Found::default()),
                Err(_) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            }
        }
        let mut picked = None;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("key_discovery")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading("Find an SSH key on this PC");
            ui.label("Searches .ssh, Bitvise locations, Documents, Desktop and Downloads. Select a file to use or import it manually.");
            ui.label(
                RichText::new("Bitvise's encrypted internal key store must be exported from Bitvise as an OpenSSH key first.")
                    .size(12.0)
                    .color(theme::text_dim()),
            );
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter by file name or location").desired_width(f32::INFINITY));
            if let Some(found) = &self.found {
                if found.limited {
                    ui.label(RichText::new("Search limit reached. Browse to search another location.").color(theme::warn()));
                }
                if found.keys.is_empty() {
                    ui.label("No supported private key files found.");
                }
                let filter = self.filter.to_lowercase();
                egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                    for path in &found.keys {
                        if !path.to_string_lossy().to_lowercase().contains(&filter) {
                            continue;
                        }
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(path.file_name().unwrap_or_default().to_string_lossy()).strong());
                                if ui.button("Select").clicked() {
                                    picked = Some(path.clone());
                                    close = true;
                                }
                            });
                            ui.label(RichText::new(path.display().to_string()).size(11.0).color(theme::text_dim()));
                        });
                    }
                });
                ui.collapsing("Searched locations", |ui| {
                    for path in &found.searched {
                        ui.label(path.display().to_string());
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Searching…");
                });
            }
            ui.horizontal(|ui| {
                if ui.button("Browse…").clicked() {
                    if let Some(path) = rfd::FileDialog::new().set_title("Choose an OpenSSH private key").pick_file() {
                        picked = Some(path);
                        close = true;
                    }
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        (picked, close || modal.should_close())
    }
}
