//! Editable path bar: Enter navigates, Esc reverts, live suggestions while typing (dim
//! ghost after the caret plus a ranked dropdown; Right / Tab accept, Up / Down pick),
//! Tab completion against the live listing as fallback (repeat to cycle), and an optional
//! history dropdown (chevron, Down arrow, or focusing an empty field).

use std::time::{Duration, Instant};

use eframe::egui::{self, CornerRadius, FontId, Id, Key, Modifiers, Rect, Sense, Stroke, Ui, Vec2};

use super::autocomplete::{self as ac, popup, popup_row};
use super::path_complete::{self as pc, Candidate, Outcome, PathStyle};
use super::suggest;
use crate::theme;

/// Listings older than this are fetched again before completing.
const LISTING_TTL: Duration = Duration::from_secs(5);
const NO_MATCH_FLASH: f64 = 0.45;
const CHEVRON_W: f32 = 22.0;
/// Quiet time after the last keystroke before the typed directory is prefetched.
const PREFETCH_DEBOUNCE: f64 = 0.15;

pub enum PathEvent {
    /// Go to this path (Enter, or a history entry / suggestion was picked).
    Navigate(String),
    /// Completion needs the entries of this directory: answer with [`PathField::provide_listing`].
    NeedListing(String),
    /// The user is typing inside this (absolute) directory: list it in the background if
    /// it is not cached, so suggestions can include its children.
    Prefetch(String),
}

/// Where live suggestions come from. `rank` gets the typed text made absolute and must
/// answer from memory (no network). A changed `version` recomputes the current list.
pub struct Suggest<'a> {
    pub version: u64,
    pub rank: &'a dyn Fn(&str) -> Vec<String>,
}

struct Cycle {
    dir: String,
    candidates: Vec<Candidate>,
    index: Option<usize>,
    /// Text we last wrote; typing anything else ends the cycle.
    shown: String,
}

struct Pending {
    /// Directory asked for (what `provide_listing` answers with).
    list_dir: String,
    /// Typed directory part the completion is appended to.
    dir: String,
    prefix: String,
    text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Popup {
    None,
    History,
    Candidates,
    Suggestions,
}

#[derive(Default)]
struct Live {
    list: Vec<String>,
    computed_for: Option<(String, u64)>,
    sel: Option<usize>,
    /// The user edited the text since focusing the field.
    typed: bool,
    /// Esc closed the dropdown; typing reopens it.
    closed: bool,
    last_edit: f64,
    prefetched: Option<String>,
}

pub struct PathField {
    pub text: String,
    /// Launch forms retain edits when another control takes focus.
    pub retain_edits: bool,
    id: Id,
    style: PathStyle,
    cycle: Option<Cycle>,
    pending: Option<Pending>,
    listing: Option<(String, Vec<Candidate>, Instant)>,
    history_open: bool,
    history_sel: Option<usize>,
    live: Live,
    cursor_to_end: bool,
    /// Set when a completion found nothing; turned into a timed red flash.
    no_match: bool,
    no_match_at: Option<f64>,
    popup_rect: Option<Rect>,
}

impl PathField {
    pub fn new(id: Id, style: PathStyle) -> Self {
        Self {
            text: String::new(),
            retain_edits: false,
            id,
            style,
            cycle: None,
            pending: None,
            listing: None,
            history_open: false,
            history_sel: None,
            live: Live::default(),
            cursor_to_end: false,
            no_match: false,
            no_match_at: None,
            popup_rect: None,
        }
    }

    pub fn id(&self) -> Id {
        self.id
    }

    /// Answers a [`PathEvent::NeedListing`]; completes right away if Tab is still pending.
    pub fn provide_listing(&mut self, dir: String, candidates: Vec<Candidate>) {
        if let Some(p) = self.pending.take_if(|p| p.list_dir == dir)
            && p.text == self.text
        {
            self.apply_completion(&p.dir, &p.prefix, &candidates);
        }
        self.listing = Some((dir, candidates, Instant::now()));
    }

    /// Drops a pending completion whose listing failed.
    pub fn listing_failed(&mut self, dir: &str) {
        if self.pending.take_if(|p| p.list_dir == dir).is_some() {
            self.no_match = true;
        }
    }

    fn popup_hovered(&self, ui: &Ui) -> bool {
        self.popup_rect.is_some_and(|r| ac::pointer_in(ui, r))
    }

    fn absolute(&self, cwd: &str) -> String {
        pc::absolutize(cwd, &self.text, self.style)
    }

    /// The suggestion the ghost / Tab / Right would take.
    fn live_choice(&self) -> Option<&String> {
        self.live.list.get(self.live.sel.unwrap_or(0))
    }

    fn ghost(&self, cwd: &str) -> Option<String> {
        if !self.live_active() || self.live.sel.is_some() {
            return None;
        }
        suggest::path_ghost(&self.absolute(cwd), self.live_choice()?, self.style)
    }

    fn live_active(&self) -> bool {
        self.live.typed && !self.live.closed && !self.live.list.is_empty() && !self.history_open && self.cycle.is_none()
    }

    pub fn show(&mut self, ui: &mut Ui, cwd: &str, history: Option<&[String]>, width: f32, suggestions: Option<Suggest<'_>>) -> Vec<PathEvent> {
        let mut events = Vec::new();
        let focused = ui.memory(|m| m.has_focus(self.id));
        if !focused && !self.popup_hovered(ui) {
            if !self.retain_edits && self.text != cwd {
                self.text = cwd.to_string();
            }
            self.cycle = None;
            self.pending = None;
            self.live = Live::default();
        }
        let history = history.unwrap_or_default();
        if focused {
            self.handle_keys(ui, cwd, history, &mut events);
        }
        if std::mem::take(&mut self.cursor_to_end) {
            ac::move_caret_to_end(ui.ctx(), self.id, &self.text);
        }

        let now = ui.input(|i| i.time);
        if std::mem::take(&mut self.no_match) {
            self.no_match_at = Some(now);
        }
        let flash = self.no_match_at.map_or(0.0, |t| (1.0 - (now - t) / NO_MATCH_FLASH).clamp(0.0, 1.0)) as f32;
        if flash > 0.0 {
            ui.ctx().request_repaint();
        }
        let text_color = theme::text().lerp_to_gamma(theme::danger(), flash);
        let field_w = if history.is_empty() { width } else { width - CHEVRON_W - 2.0 };
        let out = egui::TextEdit::singleline(&mut self.text)
            .id(self.id)
            .lock_focus(true)
            .desired_width(field_w)
            .font(egui::TextStyle::Monospace)
            .text_color(text_color)
            .margin(egui::Margin::symmetric(8, 4))
            .show(ui);
        let mut resp = out.response.response.clone();
        if !focused && !self.text.is_empty() && out.galley.size().x + 16.0 > resp.rect.width() {
            resp = resp.on_hover_text(self.text.as_str());
        }
        let mut anchor = resp.rect;
        if resp.changed() {
            self.cycle = None;
            self.history_sel = None;
            self.history_open = self.text.is_empty() && !history.is_empty();
            self.live.typed = !self.text.is_empty();
            self.live.closed = false;
            self.live.sel = None;
            self.live.last_edit = now;
        }
        if resp.gained_focus() && self.text.is_empty() && !history.is_empty() {
            self.history_open = true;
        }
        self.update_live(ui, cwd, now, suggestions, &mut events);
        if let Some(g) = self.ghost(cwd) {
            ac::paint_ghost(ui, &out, &self.text, &g);
        }
        if resp.lost_focus() {
            if ui.input(|i| i.key_pressed(Key::Enter)) && !self.text.trim().is_empty() {
                events.push(PathEvent::Navigate(self.text.trim().to_string()));
            } else if ui.input(|i| i.key_pressed(Key::Escape)) {
                self.text = cwd.to_string();
            }
        }
        if !history.is_empty() {
            let chevron = chevron(ui, self.history_open);
            anchor = anchor.union(chevron.rect);
            if chevron.on_hover_text("Recent folders").clicked() {
                self.history_open = !self.history_open;
                self.history_sel = None;
            }
        }

        let engaged = focused || self.popup_hovered(ui);
        let popup = if self.history_open && !history.is_empty() {
            Popup::History
        } else if engaged && self.cycle.as_ref().is_some_and(|c| c.shown == self.text && c.candidates.len() > 1) {
            Popup::Candidates
        } else if engaged && self.live_active() {
            Popup::Suggestions
        } else {
            Popup::None
        };
        let ctx = ui.ctx().clone();
        let fade = |name: &str, on: bool| ctx.animate_bool_with_time(self.id.with(name), on, 0.12);
        let (history_fade, complete_fade, live_fade) =
            (fade("history_fade", popup == Popup::History), fade("complete_fade", popup == Popup::Candidates), fade("live_fade", popup == Popup::Suggestions));
        self.popup_rect = match popup {
            Popup::None => None,
            Popup::History => self.history_popup(ui, anchor, history_fade, cwd, history, &mut events),
            Popup::Candidates => self.candidates_popup(ui, anchor, complete_fade),
            Popup::Suggestions => self.live_popup(ui, anchor, live_fade, &mut events),
        };
        // Close the history dropdown on a click anywhere else.
        if self.history_open && ui.input(|i| i.pointer.any_pressed()) && !ui.rect_contains_pointer(anchor) && !self.popup_hovered(ui) {
            self.history_open = false;
        }
        if events.iter().any(|e| matches!(e, PathEvent::Navigate(_))) {
            self.history_open = false;
            self.cycle = None;
            self.live = Live::default();
            // Picked from a dropdown: leave the field so it follows the new cwd.
            ui.memory_mut(|m| m.surrender_focus(self.id));
        }
        events
    }

    /// Recomputes the suggestion list when the text or the source changed, and emits a
    /// debounced prefetch of the directory being typed into.
    fn update_live(&mut self, ui: &Ui, cwd: &str, now: f64, suggestions: Option<Suggest<'_>>, events: &mut Vec<PathEvent>) {
        if !self.live.typed {
            self.live.list.clear();
            self.live.computed_for = None;
            return;
        }
        let query = self.absolute(cwd);
        if let Some(s) = suggestions {
            let key = (query.clone(), s.version);
            if self.live.computed_for.as_ref() != Some(&key) {
                self.live.list = (s.rank)(&query);
                self.live.sel = self.live.sel.filter(|&i| i < self.live.list.len());
                self.live.computed_for = Some(key);
            }
        }
        let wait = PREFETCH_DEBOUNCE - (now - self.live.last_edit);
        if wait > 0.0 {
            ui.ctx().request_repaint_after(Duration::from_secs_f64(wait));
            return;
        }
        let (dir, _) = pc::split(&query, self.style);
        let dir = pc::listing_dir(dir, self.style);
        if !dir.is_empty() && self.live.prefetched.as_deref() != Some(dir.as_str()) {
            self.live.prefetched = Some(dir.clone());
            events.push(PathEvent::Prefetch(dir));
        }
    }

    /// Puts a suggestion into the field with a trailing separator, so its children follow.
    fn take_suggestion(&mut self, path: &str) {
        self.text = pc::with_trailing_sep(path, self.style);
        self.live.sel = None;
        self.live.last_edit = 0.0;
        self.cursor_to_end = true;
    }

    fn handle_keys(&mut self, ui: &mut Ui, cwd: &str, history: &[String], events: &mut Vec<PathEvent>) {
        if self.live_active() && self.live_keys(ui, cwd, events) {
            return;
        }
        let (back, tab) = ui.input_mut(|i| {
            let back = i.consume_key(Modifiers::SHIFT, Key::Tab);
            (back, back || i.consume_key(Modifiers::NONE, Key::Tab))
        });
        if tab {
            self.history_open = false;
            if let Some(e) = self.on_tab(cwd, back) {
                events.push(e);
            }
        }
        if self.history_open && !history.is_empty() {
            let (down, up, enter, esc) = ui.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::ArrowDown),
                    i.consume_key(Modifiers::NONE, Key::ArrowUp),
                    self.history_sel.is_some() && i.consume_key(Modifiers::NONE, Key::Enter),
                    i.consume_key(Modifiers::NONE, Key::Escape),
                )
            });
            let last = history.len() - 1;
            if down {
                self.history_sel = Some(self.history_sel.map_or(0, |i| (i + 1).min(last)));
            }
            if up {
                self.history_sel = self.history_sel.and_then(|i| i.checked_sub(1));
            }
            if enter && let Some(path) = self.history_sel.and_then(|i| history.get(i)) {
                events.push(PathEvent::Navigate(path.clone()));
            }
            if esc {
                self.history_open = false;
            }
        } else if !history.is_empty() && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
            self.history_open = true;
            self.history_sel = Some(0);
        } else if self.cycle.is_some() && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.cycle = None;
        }
    }

    /// Keys while the suggestion dropdown is open; returns whether one was handled.
    fn live_keys(&mut self, ui: &mut Ui, cwd: &str, events: &mut Vec<PathEvent>) -> bool {
        let len = self.live.list.len();
        let at_end = ac::caret_at_end(ui.ctx(), self.id, &self.text);
        let has_ghost = self.ghost(cwd).is_some();
        let has_sel = self.live.sel.is_some();
        let (down, up, esc, enter, accept) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Escape),
                has_sel && i.consume_key(Modifiers::NONE, Key::Enter),
                (has_sel || has_ghost) && (i.consume_key(Modifiers::NONE, Key::Tab) || (at_end && i.consume_key(Modifiers::NONE, Key::ArrowRight))),
            )
        });
        if down || up {
            self.live.sel = ac::step(self.live.sel, len, down);
        }
        if esc {
            self.live.closed = true;
        }
        if enter && let Some(path) = self.live_choice().cloned() {
            events.push(PathEvent::Navigate(path));
        }
        if accept && let Some(path) = self.live_choice().cloned() {
            self.take_suggestion(&path);
        }
        down || up || esc || enter || accept
    }

    fn on_tab(&mut self, cwd: &str, backwards: bool) -> Option<PathEvent> {
        if let Some(c) = self.cycle.as_mut().filter(|c| c.shown == self.text && !c.candidates.is_empty()) {
            let i = pc::next_index(c.index, c.candidates.len(), backwards);
            c.index = Some(i);
            self.text = pc::completion_text(&c.dir, &c.candidates[i], self.style);
            c.shown = self.text.clone();
            self.cursor_to_end = true;
            return None;
        }
        let (dir, prefix) = pc::split(&self.text, self.style);
        let dir = pc::absolutize(cwd, dir, self.style);
        let prefix = prefix.to_string();
        let list_dir = pc::listing_dir(&dir, self.style);
        let fresh = self.listing.as_ref().filter(|(d, _, at)| *d == list_dir && at.elapsed() < LISTING_TTL);
        if let Some((_, cands, _)) = fresh {
            let cands = cands.clone();
            self.apply_completion(&dir, &prefix, &cands);
            return None;
        }
        self.pending = Some(Pending { list_dir: list_dir.clone(), dir, prefix, text: self.text.clone() });
        Some(PathEvent::NeedListing(list_dir))
    }

    fn apply_completion(&mut self, dir: &str, prefix: &str, candidates: &[Candidate]) {
        match pc::complete(dir, prefix, candidates, self.style) {
            Outcome::NoMatch => self.no_match = true,
            Outcome::Unique(text) => {
                self.text = text;
                self.cycle = None;
            }
            Outcome::Ambiguous { text, candidates } => {
                self.text = text.clone();
                self.cycle = Some(Cycle { dir: dir.to_string(), candidates, index: None, shown: text });
            }
        }
        self.live.typed = false;
        self.cursor_to_end = true;
    }

    fn history_popup(&mut self, ui: &Ui, anchor: Rect, fade: f32, cwd: &str, history: &[String], events: &mut Vec<PathEvent>) -> Option<Rect> {
        let sel = self.history_sel;
        let rect = popup(ui, self.id.with("history"), anchor, fade, egui::Order::Foreground, |ui| {
            ui.label(egui::RichText::new("RECENT").size(10.0).strong().color(theme::text_faint()));
            for (i, path) in history.iter().enumerate() {
                let color = if path == cwd { theme::accent() } else { theme::text() };
                if popup_row(ui, "📁", path, color, sel == Some(i), FontId::monospace(12.5)) {
                    events.push(PathEvent::Navigate(path.clone()));
                }
            }
        });
        Some(rect)
    }

    fn live_popup(&mut self, ui: &Ui, anchor: Rect, fade: f32, events: &mut Vec<PathEvent>) -> Option<Rect> {
        let sel = self.live.sel;
        let list = &self.live.list;
        let rect = popup(ui, self.id.with("live"), anchor, fade, egui::Order::Foreground, |ui| {
            for (i, path) in list.iter().enumerate() {
                if popup_row(ui, "📁", path, theme::text(), sel == Some(i), FontId::monospace(12.5)) {
                    events.push(PathEvent::Navigate(path.clone()));
                }
            }
        });
        Some(rect)
    }

    fn candidates_popup(&mut self, ui: &Ui, anchor: Rect, fade: f32) -> Option<Rect> {
        let cycle = self.cycle.as_ref()?;
        let mut picked = None;
        let rect = popup(ui, self.id.with("complete"), anchor, fade, egui::Order::Foreground, |ui| {
            for (i, c) in cycle.candidates.iter().enumerate() {
                let (icon, color) = if c.is_dir { ("📁", theme::text()) } else { ("📄", theme::text_dim()) };
                if popup_row(ui, icon, &c.name, color, cycle.index == Some(i), FontId::monospace(12.5)) {
                    picked = Some(i);
                }
            }
        });
        if let Some(i) = picked {
            let c = &cycle.candidates[i];
            self.text = pc::completion_text(&cycle.dir, c, self.style);
            self.cycle = None;
            self.cursor_to_end = true;
            ui.memory_mut(|m| m.request_focus(self.id));
        }
        Some(rect)
    }
}

/// Small painted chevron (no font glyph needed) that rotates when open.
fn chevron(ui: &mut Ui, open: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(CHEVRON_W, CHEVRON_W), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, open, 0.15);
    let hover = ui.ctx().animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.12);
    if hover > 0.0 {
        ui.painter().rect_filled(rect, CornerRadius::same(theme::RADIUS_SM), theme::surface_2().gamma_multiply(hover));
    }
    let c = rect.center();
    let (w, h) = (4.0, 2.5 * (1.0 - 2.0 * t));
    let color = if open { theme::accent() } else { theme::text_dim() };
    let stroke = Stroke::new(1.5, color);
    ui.painter().line_segment([c + Vec2::new(-w, -h), c + Vec2::new(0.0, h)], stroke);
    ui.painter().line_segment([c + Vec2::new(0.0, h), c + Vec2::new(w, -h)], stroke);
    resp
}
