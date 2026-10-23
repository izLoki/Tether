//! Editor dock tab for one remote file: code area, find bar, status bar, Ctrl+S upload.

use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Align, Color32, CornerRadius, FontId, Id, Key, KeyboardShortcut, Layout, Modifiers, Rect, RichText, Sense, Ui, Vec2};
use uuid::Uuid;

use super::code_editor::{self, CodeArea, Look};
use super::sftp_chrome::icon_button;
use crate::commands::EditorDoc;
use crate::remote_edit::{self, Refusal};
use crate::settings;
use crate::ssh::sftp::Sftp;
use crate::theme;

static NEXT_EDITOR: AtomicU64 = AtomicU64::new(1);
const STATUS_H: f32 = 24.0;
const TOP_H: f32 = 28.0;
const FLASH_SECS: f64 = 2.4;
const FIND_W: f32 = 330.0;
const SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const FIND: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::F);

#[derive(Debug, Clone, PartialEq)]
enum SaveStatus {
    Idle,
    Saving,
    Saved(f64),
    Failed(String),
}

/// Result of an upload, with the text that was uploaded.
type SaveSlot = Arc<Mutex<Option<(Result<(), String>, String)>>>;

#[derive(Default)]
struct Find {
    open: bool,
    query: String,
    case_sensitive: bool,
    matches: Vec<Range<usize>>,
    current: Option<usize>,
    /// (text revision, query, case) the matches were computed for.
    key: Option<(u64, String, bool)>,
    focus: bool,
    /// Set once the user steps through results; ←/→ then step too (until they type).
    browsing: bool,
}

/// Which way an arrow key steps through find results, if it does: ↑/↓ always, ←/→ only
/// while browsing (otherwise they move the caret in the query).
fn arrow_step(key: Key, browsing: bool) -> Option<bool> {
    match key {
        Key::ArrowDown => Some(true),
        Key::ArrowUp => Some(false),
        Key::ArrowRight if browsing => Some(true),
        Key::ArrowLeft if browsing => Some(false),
        _ => None,
    }
}

pub struct EditorView {
    n: u64,
    host: Uuid,
    host_label: String,
    remote_path: String,
    file_name: String,
    language: Option<String>,
    text: String,
    saved: String,
    crlf: bool,
    bom: bool,
    /// Decode failed; the text is a lossy preview and cannot be saved.
    read_only: Option<Refusal>,
    revision: u64,
    dirty: bool,
    sftp: Arc<Sftp>,
    pipeline_kb: u32,
    status: SaveStatus,
    slot: SaveSlot,
    word_wrap: bool,
    find: Find,
    select: Option<Range<usize>>,
    focus_text: bool,
    cursor: (usize, usize),
    cursor_key: (u64, usize),
    confirm_close: bool,
    close_after_save: bool,
    rt: tokio::runtime::Handle,
    ctx: egui::Context,
}

/// What the tab asks of its host after a frame.
#[derive(Default)]
pub struct EditorOutcome {
    pub close: bool,
}

impl EditorView {
    pub fn new(doc: EditorDoc, rt: tokio::runtime::Handle, ctx: egui::Context) -> Self {
        let file_name = doc.remote_path.rsplit('/').next().unwrap_or(&doc.remote_path).to_string();
        let (text, crlf, bom, read_only) = match remote_edit::decode(&doc.bytes) {
            Ok(d) => (d.text, d.crlf, d.bom, None),
            Err(r) => (String::from_utf8_lossy(&doc.bytes).into_owned(), false, false, Some(r)),
        };
        let first_line = text.lines().next().unwrap_or("");
        let language = code_editor::language_for(&file_name, first_line);
        Self {
            n: NEXT_EDITOR.fetch_add(1, Ordering::Relaxed),
            host: doc.profile.id,
            host_label: doc.profile.display_name().to_string(),
            remote_path: doc.remote_path,
            file_name,
            language,
            saved: text.clone(),
            text,
            crlf,
            bom,
            read_only,
            revision: 0,
            dirty: false,
            sftp: doc.sftp,
            pipeline_kb: doc.pipeline_kb,
            status: SaveStatus::Idle,
            slot: SaveSlot::default(),
            word_wrap: settings::get().editor.word_wrap,
            find: Find::default(),
            select: None,
            focus_text: true,
            cursor: (1, 1),
            cursor_key: (u64::MAX, 0),
            confirm_close: false,
            close_after_save: false,
            rt,
            ctx,
        }
    }

    /// True if this tab edits `remote_path` on host `profile_id`.
    pub fn edits(&self, profile_id: Uuid, remote_path: &str) -> bool {
        self.host == profile_id && self.remote_path == remote_path
    }

    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    pub fn remote_path(&self) -> &str {
        &self.remote_path
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Colour of the tab's status dot.
    pub fn status_color(&self) -> Color32 {
        match self.status {
            SaveStatus::Failed(_) => theme::danger(),
            _ if self.dirty => theme::warn(),
            SaveStatus::Saving => theme::blue(),
            _ => theme::accent(),
        }
    }

    /// Called when the tab's close button is pressed: closes at once unless there are
    /// unsaved changes, in which case a confirmation is shown.
    pub fn request_close(&mut self) -> bool {
        if self.dirty {
            self.confirm_close = true;
            return false;
        }
        true
    }

    fn text_id(&self) -> Id {
        Id::new(("editor_text", self.n))
    }

    fn find_id(&self) -> Id {
        Id::new(("editor_find", self.n))
    }

    fn save(&mut self) {
        if self.read_only.is_some() || self.status == SaveStatus::Saving {
            return;
        }
        let snapshot = self.text.clone();
        let bytes = remote_edit::encode(&snapshot, self.crlf, self.bom);
        self.status = SaveStatus::Saving;
        let (sftp, path, kb, slot, ctx) = (self.sftp.clone(), self.remote_path.clone(), self.pipeline_kb, self.slot.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let result = remote_edit::upload_atomic(&sftp, &path, &bytes, kb).await.map_err(|e| format!("{e:#}"));
            if let Ok(mut s) = slot.lock() {
                *s = Some((result, snapshot));
            }
            ctx.request_repaint();
        });
    }

    /// Applies a finished upload. Returns true when a pending close may now happen.
    fn poll_save(&mut self, now: f64) -> bool {
        let Some((result, snapshot)) = self.slot.lock().ok().and_then(|mut s| s.take()) else { return false };
        match result {
            Ok(()) => {
                self.saved = snapshot;
                self.dirty = self.text != self.saved;
                self.status = SaveStatus::Saved(now);
                std::mem::take(&mut self.close_after_save) && !self.dirty
            }
            Err(e) => {
                self.status = SaveStatus::Failed(e);
                self.close_after_save = false;
                false
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) -> EditorOutcome {
        let now = ui.input(|i| i.time);
        let mut outcome = EditorOutcome { close: self.poll_save(now) };
        self.shortcuts(ui);
        let full = ui.available_rect_before_wrap();
        let top = Rect::from_min_size(full.min, Vec2::new(full.width(), TOP_H));
        let status = Rect::from_min_max(egui::pos2(full.left(), full.bottom() - STATUS_H), full.max);
        let body = Rect::from_min_max(egui::pos2(full.left(), top.bottom()), egui::pos2(full.right(), status.top()));

        self.top_bar(ui, top);
        self.code(ui, body);
        if self.find.open {
            self.find_bar(ui, body);
        }
        self.status_bar(ui, status, now);
        self.flash(ui, body, now);
        outcome.close |= self.close_dialog(ui.ctx());
        ui.allocate_rect(full, Sense::hover());
        outcome
    }

    fn shortcuts(&mut self, ui: &Ui) {
        let focused = ui.memory(|m| m.focused());
        if focused != Some(self.text_id()) && focused != Some(self.find_id()) {
            return;
        }
        let (save, find, escape) = ui.ctx().input_mut(|i| {
            // Exactly Ctrl+S / Ctrl+F: Ctrl+Shift+F etc. stay with the app's shortcuts.
            let plain = !i.modifiers.shift && !i.modifiers.alt;
            (plain && i.consume_shortcut(&SAVE), plain && i.consume_shortcut(&FIND), self.find.open && i.key_pressed(Key::Escape))
        });
        if save {
            self.save();
        }
        if find {
            self.open_find();
        }
        if escape {
            self.find.open = false;
            self.focus_text = true;
        }
    }

    fn open_find(&mut self) {
        let sel = TextSel::current(self.text_id(), &self.ctx);
        if let Some(r) = sel.filter(|r| r.end > r.start && r.end - r.start < 200) {
            let picked: String = self.text.chars().skip(r.start).take(r.end - r.start).collect();
            if !picked.contains('\n') {
                self.find.query = picked;
            }
        }
        self.find.open = true;
        self.find.focus = true;
    }

    fn top_bar(&mut self, ui: &mut Ui, rect: Rect) {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(10.0, 0.0))).layout(Layout::left_to_right(Align::Center)));
        child.spacing_mut().item_spacing.x = 4.0;
        child.label(RichText::new(&self.host_label).size(11.5).color(theme::text_faint()));
        let parts: Vec<&str> = self.remote_path.split('/').filter(|s| !s.is_empty()).collect();
        for (i, part) in parts.iter().enumerate() {
            child.label(RichText::new("›").size(11.5).color(theme::text_faint()));
            let color = if i + 1 == parts.len() { theme::text() } else { theme::text_dim() };
            child.label(RichText::new(*part).size(11.5).color(color));
        }
        child.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if self.read_only.is_none() && icon_button(ui, "💾", &format!("Save to server ({})", crate::platform::shortcut("S"))) {
                self.save();
            }
            if icon_button(ui, "🔍", &format!("Find ({})", crate::platform::shortcut("F"))) {
                self.open_find();
            }
            let wrap_tip = if self.word_wrap { "Word wrap: on" } else { "Word wrap: off" };
            let wrap = ui.add(
                egui::Button::new(RichText::new("↩").color(if self.word_wrap { theme::accent() } else { theme::text_dim() }))
                    .frame(false)
                    .min_size(Vec2::splat(24.0)),
            );
            if wrap.on_hover_text(wrap_tip).clicked() {
                self.word_wrap = !self.word_wrap;
            }
        });
        ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, egui::Stroke::new(1.0, theme::border()));
    }

    fn code(&mut self, ui: &mut Ui, rect: Rect) {
        let prefs = settings::get().editor;
        let look = Look {
            font: FontId::monospace(prefs.font_size.clamp(8.0, 40.0)),
            dark: theme::palette().dark,
            wrap: self.word_wrap,
            language: self.language.clone(),
        };
        self.refresh_matches();
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(("editor_body", self.n)));
        child.painter().rect_filled(rect, CornerRadius::ZERO, theme::surface());
        let (select, focus) = (self.select.take(), std::mem::take(&mut self.focus_text));
        let read_only = self.read_only.is_some();
        let out = egui::ScrollArea::new([!self.word_wrap, true])
            .id_salt(("editor_scroll", self.n))
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                let mut scratch;
                let text = if read_only {
                    scratch = self.text.clone();
                    &mut scratch
                } else {
                    &mut self.text
                };
                code_editor::show(
                    ui,
                    CodeArea {
                        id: Id::new(("editor_text", self.n)),
                        text,
                        look: &look,
                        tab_size: prefs.tab_size,
                        line_numbers: prefs.line_numbers,
                        matches: &self.find.matches,
                        current_match: self.find.current,
                        select,
                        focus,
                    },
                )
            })
            .inner;
        if out.changed && !read_only {
            self.revision += 1;
            self.dirty = self.text != self.saved;
            if matches!(self.status, SaveStatus::Saved(_) | SaveStatus::Failed(_)) && self.dirty {
                self.status = SaveStatus::Idle;
            }
        }
        if let Some(c) = out.cursor
            && self.cursor_key != (self.revision, c)
        {
            self.cursor_key = (self.revision, c);
            self.cursor = code_editor::line_col(&self.text, c);
        }
    }

    fn refresh_matches(&mut self) {
        let key = (self.revision, self.find.query.clone(), self.find.case_sensitive);
        if !self.find.open || self.find.key.as_ref() == Some(&key) {
            return;
        }
        self.find.matches = code_editor::find_matches(&self.text, &self.find.query, self.find.case_sensitive);
        self.find.current = (!self.find.matches.is_empty()).then(|| self.find.current.unwrap_or(0).min(self.find.matches.len() - 1));
        self.find.key = Some(key);
    }

    fn step_match(&mut self, forward: bool) {
        let n = self.find.matches.len();
        if n == 0 {
            return;
        }
        let i = match self.find.current {
            Some(c) if forward => (c + 1) % n,
            Some(c) => (c + n - 1) % n,
            None => 0,
        };
        self.find.current = Some(i);
        self.select = Some(self.find.matches[i].clone());
    }

    fn find_bar(&mut self, ui: &mut Ui, body: Rect) {
        let w = FIND_W.min(body.width() - 24.0).max(160.0);
        let rect = Rect::from_min_size(egui::pos2(body.right() - w - 18.0, body.top() + 6.0), Vec2::new(w, 34.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)));
        let p = child.painter();
        let shadow = egui::epaint::Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(90) };
        p.add(shadow.as_shape(rect, CornerRadius::same(theme::RADIUS_SM)));
        p.rect_filled(rect, CornerRadius::same(theme::RADIUS_SM), theme::surface_3());
        child.add_space(8.0);
        let before = self.find.query.clone();
        let find_id = self.find_id();
        // Arrow keys step through results; taken before the field would move its caret.
        if child.memory(|m| m.has_focus(find_id)) {
            let browsing = self.find.browsing;
            let steps: Vec<bool> = child.input_mut(|i| {
                let mut steps = Vec::new();
                i.events.retain(|e| match e {
                    egui::Event::Key { key, pressed, modifiers, .. } if modifiers.is_none() => match arrow_step(*key, browsing) {
                        Some(forward) => {
                            if *pressed {
                                steps.push(forward);
                            }
                            false
                        }
                        None => true,
                    },
                    _ => true,
                });
                steps
            });
            for forward in steps {
                self.step_match(forward);
                self.find.browsing = true;
            }
        }
        let field =
            child.add(egui::TextEdit::singleline(&mut self.find.query).id(find_id).hint_text("Find").desired_width(w - 170.0).font(FontId::proportional(12.5)));
        if std::mem::take(&mut self.find.focus) {
            field.request_focus();
        }
        let (enter, shift) = child.input(|i| (i.key_pressed(Key::Enter), i.modifiers.shift));
        if field.lost_focus() && enter {
            self.step_match(!shift);
            self.find.browsing = true;
            field.request_focus();
        }
        if self.find.query != before {
            self.find.browsing = false;
            self.find.current = None;
            self.refresh_matches();
            if let Some(m) = self.find.matches.first() {
                self.find.current = Some(0);
                self.select = Some(m.clone());
            }
        }
        let count = match (self.find.current, self.find.matches.len()) {
            (_, 0) if !self.find.query.is_empty() => "No results".to_string(),
            (_, 0) => String::new(),
            (Some(c), n) => format!("{} of {n}", c + 1),
            (None, n) => format!("{n} found"),
        };
        let color = if count == "No results" { theme::danger() } else { theme::text_dim() };
        child.label(RichText::new(count).size(11.0).color(color));
        child.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(4.0);
            if icon_button(ui, "✕", "Close (Esc)") {
                self.find.open = false;
                self.focus_text = true;
            }
            if icon_button(ui, "↓", "Next match (Enter, ↓, →)") {
                self.step_match(true);
            }
            if icon_button(ui, "↑", "Previous match (Shift+Enter, ↑, ←)") {
                self.step_match(false);
            }
            let case = if self.find.case_sensitive { theme::accent() } else { theme::text_dim() };
            let aa = ui.add(egui::Button::new(RichText::new("Aa").size(11.5).color(case)).frame(false).min_size(Vec2::splat(24.0)));
            if aa.on_hover_text("Match case").clicked() {
                self.find.case_sensitive = !self.find.case_sensitive;
            }
        });
    }

    fn status_bar(&mut self, ui: &mut Ui, rect: Rect, now: f64) {
        let p = ui.painter();
        p.rect_filled(rect, CornerRadius::ZERO, theme::bg());
        p.hline(rect.x_range(), rect.top() + 0.5, egui::Stroke::new(1.0, theme::border()));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(10.0, 0.0))).layout(Layout::left_to_right(Align::Center)));
        child.spacing_mut().item_spacing.x = 14.0;
        self.save_indicator(&mut child, now);
        let small = |t: String| RichText::new(t).size(11.0).color(theme::text_dim());
        child.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(small(self.language.clone().unwrap_or_else(|| "Plain Text".into())));
            ui.label(small(if self.crlf { "CRLF" } else { "LF" }.into()));
            ui.label(small(if self.bom { "UTF-8 with BOM" } else { "UTF-8" }.into()));
            ui.label(small(format!("Spaces: {}", settings::get().editor.tab_size)));
            ui.label(small(format!("Ln {}, Col {}", self.cursor.0, self.cursor.1)));
        });
    }

    fn save_indicator(&self, ui: &mut Ui, now: f64) {
        let (label, color, pulse, tip) = match &self.status {
            _ if self.read_only.is_some() => ("Read-only", theme::text_faint(), false, self.read_only.as_ref().map(ToString::to_string)),
            SaveStatus::Saving => ("Saving…", theme::blue(), true, None),
            SaveStatus::Failed(e) => ("Save failed", theme::danger(), false, Some(e.clone())),
            _ if self.dirty => ("Unsaved", theme::warn(), false, Some(format!("{} uploads to the server", crate::platform::shortcut("S")))),
            _ => ("Saved", theme::accent(), false, None),
        };
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), Sense::hover());
        let alpha = if pulse {
            ui.ctx().request_repaint();
            0.45 + 0.55 * ((now * 5.0).sin() * 0.5 + 0.5) as f32
        } else {
            1.0
        };
        let r = if pulse { 3.0 + alpha } else { 3.5 };
        ui.painter().circle_filled(rect.center(), r, color.gamma_multiply(alpha));
        let label = ui.label(RichText::new(label).size(11.0).color(color));
        if let Some(t) = tip {
            resp.union(label).on_hover_text(t);
        }
    }

    /// Brief pill in the top-right corner after a save finishes.
    fn flash(&self, ui: &Ui, body: Rect, now: f64) {
        let (text, color, at) = match &self.status {
            SaveStatus::Saved(at) => (format!("✔  Saved to {}", self.host_label), theme::accent(), *at),
            _ => return,
        };
        let age = now - at;
        if age > FLASH_SECS {
            return;
        }
        ui.ctx().request_repaint();
        let alpha = (age / 0.18).min(1.0).min((FLASH_SECS - age) / 0.4) as f32;
        let p = ui.painter_at(body);
        let galley = p.layout_no_wrap(text, FontId::proportional(12.0), theme::text());
        let size = galley.size() + Vec2::new(24.0, 12.0);
        let y = body.top() + 8.0 + if self.find.open { 40.0 } else { 0.0 } - (1.0 - alpha) * 6.0;
        let rect = Rect::from_min_size(egui::pos2(body.right() - size.x - 18.0, y), size);
        p.rect_filled(rect, CornerRadius::same(12), theme::surface_3().gamma_multiply(alpha));
        p.rect_stroke(rect, CornerRadius::same(12), egui::Stroke::new(1.0, color.gamma_multiply(0.6 * alpha)), egui::StrokeKind::Inside);
        p.galley_with_override_text_color(rect.min + Vec2::new(12.0, 6.0), galley, theme::text().gamma_multiply(alpha));
    }

    /// "Save changes?" confirmation. Returns true when the tab should close now.
    fn close_dialog(&mut self, ctx: &egui::Context) -> bool {
        if !self.confirm_close {
            return false;
        }
        let mut close = false;
        let modal = egui::Modal::new(Id::new(("editor_close", self.n))).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.heading(format!("Save changes to {}?", self.file_name));
            ui.label(RichText::new("Your changes will be lost if you don't save them.").color(theme::text_dim()));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let save = egui::Button::new(RichText::new("Save").color(Color32::WHITE)).fill(theme::accent().gamma_multiply(0.75));
                if ui.add(save).clicked() {
                    self.close_after_save = true;
                    self.confirm_close = false;
                    self.save();
                }
                if ui.button("Don't save").clicked() {
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_close = false;
                }
            });
        });
        if modal.should_close() {
            self.confirm_close = false;
        }
        if close {
            self.confirm_close = false;
        }
        close
    }
}

/// Reads a TextEdit's current selection without touching the widget.
struct TextSel;

impl TextSel {
    fn current(id: Id, ctx: &egui::Context) -> Option<Range<usize>> {
        let r = egui::TextEdit::load_state(ctx, id)?.cursor.char_range()?.as_sorted_char_range();
        Some(r.start.0..r.end.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertical_arrows_always_step_horizontal_only_while_browsing() {
        assert_eq!(arrow_step(Key::ArrowDown, false), Some(true));
        assert_eq!(arrow_step(Key::ArrowUp, false), Some(false));
        assert_eq!(arrow_step(Key::ArrowRight, false), None, "→ moves the caret while typing");
        assert_eq!(arrow_step(Key::ArrowRight, true), Some(true));
        assert_eq!(arrow_step(Key::ArrowLeft, true), Some(false));
        assert_eq!(arrow_step(Key::A, true), None);
    }
}
