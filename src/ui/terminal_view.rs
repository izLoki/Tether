//! Renders a vt100 screen with egui's painter and turns input into bytes.
//!
//! Every visual knob (font size, line height, padding, cursor, bell, scrollbar)
//! is read from `settings::snapshot().terminal` each frame, so the Settings screen
//! updates open terminals live.
//!
//! The cell grid itself is cached row by row in [`GridCache`]; the terminal mutex is
//! held only while rows are hashed and new ones are prepared, never while painting.

use eframe::egui::{self, Align2, CornerRadius, Event, EventFilter, FontFamily, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use super::keys;
use super::loading::{self, ConnectInfo};
use super::term_grid::{FrameKey, GridCache, RenderKey};
use crate::keybindings::{self, Action};
use crate::profile::Profile;
use crate::settings::{self, CursorStyle, TerminalSettings};
use crate::ssh::connection::Stage;
use crate::ssh::terminal::{LinkState, TerminalHandle};
use crate::theme;

const BLINK_PERIOD: f64 = 1.1;
/// Share of the blink period the cursor is shown.
const BLINK_ON: f64 = 0.6;
const BELL_FLASH_SECS: f64 = 0.45;
const SCROLLBAR_LINGER_SECS: f64 = 1.2;
const SCROLLBAR_WIDTH: f32 = 4.0;
/// Ctrl+wheel zoom is persisted once the wheel has been idle this long.
const FONT_SAVE_DELAY: f64 = 0.6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Cell {
    row: u16,
    col: u16,
}

/// Cell metrics for a font size and line-height multiplier.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub cell_w: f32,
    pub cell_h: f32,
    /// Vertical offset of the glyphs inside a (taller) cell.
    pub text_dy: f32,
}

pub fn metrics(ctx: &egui::Context, font: &FontId, line_height: f32) -> Metrics {
    let (cell_w, glyph_h) = ctx.fonts_mut(|f| (f.glyph_width(font, 'M'), f.row_height(font)));
    let cell_h = (glyph_h * line_height).round().max(1.0);
    Metrics { cell_w, cell_h, text_dy: ((cell_h - glyph_h) * 0.5).round() }
}

pub struct TerminalView {
    /// Enabled by the tab's foreground-agent detector, independent of badge visibility.
    pub images_enabled: bool,
    pub handle: TerminalHandle,
    size: (u16, u16),
    scroll: usize,
    selection: Option<(Cell, Cell)>,
    last_activity: f64,
    wheel_acc: f32,
    last_bell: u64,
    bell_at: Option<f64>,
    scrolled_at: f64,
    font_dirty_at: Option<f64>,
    /// UI time the current connect started (drives the connecting screen).
    connecting_since: Option<f64>,
    /// Stage shown while the connecting screen fades out.
    last_stage: Stage,
    /// Cursor `(row, col)` when the user started typing on the current shell prompt.
    prompt_col: Option<(u16, u16)>,
    /// Hand `nano <file>` to the built-in SFTP editor (off for local shells).
    pub intercept_nano: bool,
    /// Take keyboard focus on the next frame (the agents panel jumping to this tab).
    pub want_focus: bool,
    /// Fallback working directory (e.g. from an AI agent hook when OSC 7 hasn't fired).
    pub fallback_cwd: Option<String>,
    /// Boxed to keep `TabBody` small.
    cache: Box<ViewCache>,
}

/// Painted rows and the last header info seen.
struct ViewCache {
    /// Clipboard and file-drop input must only be handled once per gesture/frame.
    paste_guard: super::os_input::PasteGuard,
    last_drop_frame: Option<u64>,
    /// Characters typed since the last submitted/cancelled line (fallback before echo).
    typed_line: String,
    attachments: super::attachments::Attachments,
    grid: GridCache,
    /// Last OSC title and link state seen, for the tab header (read without blocking).
    title: Option<String>,
    /// Last OSC 7 working directory seen.
    cwd: Option<String>,
    link: LinkState,
    /// Shell-integration blocks on screen (marks, selection, sticky header).
    blocks: super::blocks::BlocksView,
    /// Warp-style clickable file paths and URLs under the pointer.
    links: super::term_links::LinksState,
}

/// What a frame needs from the shared terminal state, copied out under the lock.
struct FrameInfo {
    state: LinkState,
    stage: Stage,
    bells: u64,
    history: usize,
    app_cursor: bool,
    bracketed: bool,
    alt_screen: bool,
    /// Cursor position and the glyph under it, unless the app hid the cursor.
    cursor: Option<((u16, u16), Option<String>)>,
}

#[derive(Default)]
pub struct ViewOutput {
    /// Bytes the user typed this frame (the app routes them, possibly broadcasting).
    pub input: Vec<u8>,
    pub reconnect: bool,
    /// Remote files the user asked to edit from the shell (e.g. `nano <file>`).
    pub open_files: Vec<String>,
    pub open_folders: Vec<String>,
}

impl ViewOutput {
    fn link_action(&mut self, action: Option<super::term_links::LinkAction>) {
        match action {
            Some(super::term_links::LinkAction::OpenRemote(path)) => self.open_files.push(path),
            Some(super::term_links::LinkAction::OpenSftp(path)) => self.open_folders.push(path),
            None => {}
        }
    }
}

impl TerminalView {
    pub fn attachments(&mut self) -> &mut super::attachments::Attachments {
        &mut self.cache.attachments
    }

    pub fn new(handle: TerminalHandle) -> Self {
        Self {
            images_enabled: false,
            handle,
            size: (0, 0),
            scroll: 0,
            selection: None,
            last_activity: 0.0,
            wheel_acc: 0.0,
            last_bell: 0,
            bell_at: None,
            scrolled_at: f64::NEG_INFINITY,
            font_dirty_at: None,
            connecting_since: None,
            last_stage: Stage::Connecting,
            prompt_col: None,
            intercept_nano: true,
            want_focus: false,
            fallback_cwd: None,
            cache: Box::new(ViewCache {
                paste_guard: Default::default(),
                last_drop_frame: None,
                grid: GridCache::default(),
                typed_line: String::new(),
                attachments: Default::default(),
                title: None,
                cwd: None,
                link: LinkState::Connecting,
                blocks: Default::default(),
                links: Default::default(),
            }),
        }
    }

    /// Remote (OSC) window title for the tab header. Never waits on the output reader:
    /// when it holds the lock, the last known title is returned.
    pub fn remote_title(&mut self) -> Option<String> {
        if let Ok(s) = self.handle.shared.try_lock() {
            self.cache.link.clone_from(&s.state);
            if s.parser.callbacks().title != self.cache.title {
                self.cache.title.clone_from(&s.parser.callbacks().title);
            }
            if s.parser.callbacks().cwd != self.cache.cwd {
                self.cache.cwd.clone_from(&s.parser.callbacks().cwd);
            }
            self.cache.blocks.note_branch(&s.parser.callbacks().shell);
        }
        self.cache.title.clone()
    }

    /// Working directory and git branch reported by shell integration (OSC 7 /
    /// OSC 6973), as of the latest [`Self::remote_title`] call or frame. Never blocks.
    pub fn shell_context(&self) -> Option<(&str, Option<&str>)> {
        self.cache.cwd.as_deref().map(|cwd| (cwd, self.cache.blocks.branch()))
    }

    /// Working directory (OSC 7) as of the latest [`Self::remote_title`] call.
    pub fn remote_cwd(&self) -> Option<&str> {
        self.cache.cwd.as_deref()
    }

    /// Link state as of the latest frame or [`Self::remote_title`] call.
    pub fn link_state(&self) -> &LinkState {
        &self.cache.link
    }

    pub fn ui(&mut self, ui: &mut Ui, profile: &Profile) -> ViewOutput {
        let accent = theme::hue_color(profile.hue);
        let mut out = ViewOutput::default();
        let settings = settings::snapshot();
        let cfg = &settings.terminal;
        let full_rect = ui.available_rect_before_wrap();
        let bar_height = if self.images_enabled { 34.0 } else { 0.0 };
        let bar_rect = Rect::from_min_max(egui::pos2(full_rect.left(), (full_rect.bottom() - bar_height).max(full_rect.top())), full_rect.max);
        let rect = Rect::from_min_max(full_rect.min, egui::pos2(full_rect.right(), bar_rect.top()));
        let id = ui.id().with("terminal");
        let response = ui.interact(rect, id, Sense::click_and_drag());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Terminal"));
        if response.clicked() || response.drag_started() || std::mem::take(&mut self.want_focus) {
            response.request_focus();
        }
        let focused = response.has_focus();
        // A modal (settings, dialogs) above us owns the keyboard even if we keep focus.
        let layer = ui.layer_id();
        let under_modal = ui.ctx().memory(|m| m.top_modal_layer()).is_some_and(|l| l != layer);
        let keys_active = focused && !under_modal;
        if keys_active {
            ui.memory_mut(|m| m.set_focus_lock_filter(id, EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true }));
        }
        let now = ui.ctx().input(|i| i.time);
        self.persist_font_when_idle(now);

        let font = FontId::new(cfg.font_size(), FontFamily::Monospace);
        let m = metrics(ui.ctx(), &font, cfg.line_height());
        let inner = rect.shrink(cfg.padding());
        let cols = ((inner.width() / m.cell_w).floor() as u16).max(2);
        let rows = ((inner.height() / m.cell_h).floor() as u16).max(1);
        self.apply_resize(rows, cols);

        let painter = ui.painter_at(rect);
        let palette = theme::palette();
        painter.rect_filled(rect, CornerRadius::same(theme::RADIUS), palette.term_bg);
        self.handle.repaint.mark_painted(ui.ctx());
        super::backdrop::paint(&painter, rect);

        let hover_cell = response.hovered().then(|| ui.ctx().pointer_hover_pos()).flatten().filter(|p| inner.contains(*p)).map(|p| {
            let r = (((p.y - inner.top()) / m.cell_h).floor().max(0.0) as u16).min(rows - 1);
            let c = (((p.x - inner.left()) / m.cell_w).floor().max(0.0) as u16).min(cols - 1);
            (r, c)
        });

        let Some(frame) = self.sync_grid((rows, cols), &font, &palette, RenderKey::of(ui.ctx()), hover_cell) else { return out };
        // Glyph layout for new rows happens with the lock released.
        self.cache.grid.layout(ui.ctx());
        let FrameInfo { state, stage, bells, history, app_cursor, bracketed, alt_screen, cursor } = frame;
        let ready = self.cache.attachments.poll(ui.ctx());
        if self.images_enabled && !ready.is_empty() {
            // Each image belongs to this terminal/SSH target, even during broadcast.
            self.handle.send(keys::encode_paste(&super::os_input::format_paths(&ready), bracketed));
            self.want_focus = true;
        }
        self.cache.link.clone_from(&state);

        let to_cell = |p: Pos2| Cell {
            row: (((p.y - inner.top()) / m.cell_h).floor().max(0.0) as u16).min(rows - 1),
            col: (((p.x - inner.left()) / m.cell_w).round().max(0.0) as u16).min(cols),
        };
        let scroll_before = self.scroll;
        self.handle_mouse(ui, &response, to_cell, m.cell_h, (alt_screen, app_cursor), &mut out);

        let sel = self.selection.map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
        self.cache.grid.paint(&painter, inner.min, m, |row| selection_cols(sel, row, cols));
        if !alt_screen {
            out.link_action(self.cache.links.paint_and_menu(ui, &painter, inner, m, accent));
            let geom = super::blocks::Geom { rect, inner, cell_h: m.cell_h, rows };
            if let Some(req) = self.cache.blocks.paint(ui, geom, &settings.shell_integration, accent) {
                self.block_request(req, ui.ctx(), &mut out);
            }
        }

        if self.scroll == 0
            && let Some((pos, glyph)) = &cursor
        {
            self.paint_live_cursor(ui, &painter, (*pos, glyph.as_deref()), (inner, m, &font), cfg, (id, focused));
        }

        if response.drag_stopped()
            && cfg.copy_on_select
            && let Some(text) = self.selected_text().filter(|t| !t.is_empty())
        {
            ui.ctx().copy_text(text);
        }
        if bells != self.last_bell {
            self.last_bell = bells;
            if cfg.visual_bell {
                self.bell_at = Some(now);
            }
        }
        self.paint_bell(ui.ctx(), &painter, rect, now);

        if keys_active {
            self.handle_keys(ui, app_cursor, bracketed, &mut out);
            self.paste_without_text(ui, bracketed, &mut out);
        }
        if self.file_drop(ui, rect, bracketed, &mut out) {
            response.request_focus();
        }
        if self.scroll != scroll_before {
            self.scrolled_at = now;
        }
        if cfg.scrollbar && !alt_screen {
            self.scrollbar(ui, rect, (history, rows as usize), now);
        }
        if self.scroll > 0 && self.scroll_pill(ui, rect) {
            self.scroll = 0;
        }

        let ring = ui.ctx().animate_bool_with_time(id.with("focus"), focused, theme::anim_time(0.18));
        if ring > 0.0 {
            painter.rect_stroke(rect, CornerRadius::same(theme::RADIUS), Stroke::new(1.0, accent.gamma_multiply(0.45 * ring)), egui::StrokeKind::Inside);
        }
        self.connecting_screen(ui, rect, &state, stage, profile, now);
        out.reconnect = state_overlay(ui, rect, &state);
        if self.images_enabled {
            self.cache.attachments.bar(ui, bar_rect);
        }
        if !out.input.is_empty() {
            self.scroll = 0;
            self.selection = None;
            self.last_activity = now;
        }
        out
    }

    /// Locks the terminal just long enough to clamp the scroll offset, refresh the row
    /// cache and copy out what the rest of the frame needs. `None` if the lock is poisoned.
    fn sync_grid(&mut self, size: (u16, u16), font: &FontId, palette: &theme::Palette, render: RenderKey, hover_cell: Option<(u16, u16)>) -> Option<FrameInfo> {
        let shared_arc = self.handle.shared.clone();
        let mut shared = shared_arc.lock().ok()?;
        let generation = shared.generation;
        let screen = shared.parser.screen_mut();
        // Probe the history length (O(1)): vt100 caps the offset at what it has stored.
        screen.set_scrollback(usize::MAX);
        let history = screen.scrollback();
        screen.set_scrollback(self.scroll);
        self.scroll = screen.scrollback();
        let screen = shared.parser.screen();
        self.cache.grid.sync(FrameKey::new(generation, self.scroll, size, font, render), screen, palette, font);
        let cursor = (!screen.hide_cursor()).then(|| {
            let (cr, cc) = screen.cursor_position();
            let glyph = screen.cell(cr, cc).filter(|c| c.has_contents()).map(|c| c.contents().to_string());
            ((cr, cc), glyph)
        });
        if shared.parser.callbacks().title != self.cache.title {
            self.cache.title.clone_from(&shared.parser.callbacks().title);
        }
        if shared.parser.callbacks().cwd != self.cache.cwd {
            self.cache.cwd.clone_from(&shared.parser.callbacks().cwd);
        }
        self.cache.blocks.sync(&shared.parser.callbacks().shell, self.scroll, size.0);
        let effective_cwd = self.cache.cwd.as_deref().or(self.fallback_cwd.as_deref());
        let hover = if screen.alternate_screen() { None } else { hover_cell };
        self.cache.links.sync(screen, generation, self.scroll, hover, effective_cwd, !self.intercept_nano);
        Some(FrameInfo {
            state: shared.state.clone(),
            stage: shared.stage,
            bells: shared.parser.callbacks().bell_count,
            history,
            app_cursor: screen.application_cursor(),
            bracketed: screen.bracketed_paste(),
            alt_screen: screen.alternate_screen(),
            cursor,
        })
    }

    fn paint_live_cursor(
        &self,
        ui: &Ui,
        painter: &egui::Painter,
        ((cr, cc), glyph): ((u16, u16), Option<&str>),
        (inner, m, font): (Rect, Metrics, &FontId),
        cfg: &TerminalSettings,
        (id, focused): (egui::Id, bool),
    ) {
        let target = Pos2::new(inner.left() + cc as f32 * m.cell_w, inner.top() + cr as f32 * m.cell_h);
        let ctx = ui.ctx();
        let glide = theme::anim_time(0.06);
        let x = ctx.animate_value_with_time(id.with("cx"), target.x, glide);
        let y = ctx.animate_value_with_time(id.with("cy"), target.y, glide);
        let cell = Rect::from_min_size(Pos2::new(x, y), Vec2::new(m.cell_w, m.cell_h));
        let t = ctx.input(|i| i.time);
        let blinking = focused && cfg.cursor_blink;
        let phase = (t - self.last_activity).rem_euclid(BLINK_PERIOD);
        let visible = !blinking || phase < BLINK_PERIOD * BLINK_ON;
        if visible {
            paint_cursor(painter, cell, m.text_dy, cfg.cursor_style, focused, glyph, font);
        }
        if blinking {
            // Wake exactly at the next on/off flip instead of polling.
            let next = if phase < BLINK_PERIOD * BLINK_ON { BLINK_PERIOD * BLINK_ON - phase } else { BLINK_PERIOD - phase };
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(next.max(0.01)));
        }
    }

    /// Full-pane connecting screen; appears only if the connect takes a moment and fades
    /// out onto the live shell.
    fn connecting_screen(&mut self, ui: &Ui, rect: Rect, state: &LinkState, stage: Stage, profile: &Profile, now: f64) {
        let connecting = matches!(state, LinkState::Connecting);
        let since = if connecting { *self.connecting_since.get_or_insert(now) } else { self.connecting_since.take().unwrap_or(now) };
        if connecting {
            self.last_stage = stage;
        }
        let elapsed = now - since;
        let show = connecting && elapsed > loading::SHOW_AFTER_SECS;
        if connecting && !show {
            ui.ctx().request_repaint();
        }
        let t = ui.ctx().animate_bool_with_time(ui.id().with("connecting"), show, theme::anim_time(0.3));
        if t <= 0.0 {
            return;
        }
        ui.painter_at(rect).rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::term_bg().gamma_multiply(t));
        let address = profile.address();
        let info = ConnectInfo {
            name: profile.display_name(),
            address: &address,
            hue: profile.hue,
            stage: self.last_stage,
            stages: loading::TERMINAL_STAGES,
            elapsed,
        };
        loading::connect_screen(ui, rect, &info, t);
    }

    fn paint_bell(&mut self, ctx: &egui::Context, painter: &egui::Painter, rect: Rect, now: f64) {
        let Some(at) = self.bell_at else { return };
        let k = 1.0 - ((now - at) / BELL_FLASH_SECS) as f32;
        if k <= 0.0 {
            self.bell_at = None;
            return;
        }
        let k = k * k;
        painter.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::term_fg().gamma_multiply(0.06 * k));
        painter.rect_stroke(
            rect.shrink(0.5),
            CornerRadius::same(theme::RADIUS),
            Stroke::new(1.5, theme::warn().gamma_multiply(0.7 * k)),
            egui::StrokeKind::Inside,
        );
        ctx.request_repaint();
    }

    /// Thin overlay scrollbar: fades in while scrolling or when the pointer nears the edge.
    fn scrollbar(&mut self, ui: &mut Ui, rect: Rect, (history, rows): (usize, usize), now: f64) {
        if history == 0 {
            return;
        }
        let track = Rect::from_min_max(Pos2::new(rect.right() - SCROLLBAR_WIDTH - 4.0, rect.top() + 6.0), Pos2::new(rect.right() - 4.0, rect.bottom() - 6.0));
        let grab_zone = track.expand2(Vec2::new(5.0, 0.0));
        let sb_id = ui.id().with("term_scrollbar");
        let near = ui.ctx().pointer_hover_pos().is_some_and(|p| rect.contains(p) && p.x > rect.right() - 22.0);
        let recent = now - self.scrolled_at < SCROLLBAR_LINGER_SECS;
        let show = recent || near || ui.ctx().is_being_dragged(sb_id);
        let alpha = ui.ctx().animate_bool_with_time(ui.id().with("term_sb_vis"), show, theme::anim_time(0.2));
        if recent {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        if alpha <= 0.0 {
            return;
        }
        // Only grab the pointer while visible so edge-column selections still work otherwise.
        let resp = ui.interact(grab_zone, sb_id, Sense::drag());
        let (top_frac, len_frac) = thumb_span(self.scroll, history, rows);
        let thumb_h = (track.height() * len_frac).max(24.0).min(track.height());
        let travel = track.height() - thumb_h;
        if resp.dragged()
            && let Some(p) = resp.interact_pointer_pos()
        {
            let frac = if travel > 0.0 { ((p.y - track.top() - thumb_h * 0.5) / travel).clamp(0.0, 1.0) } else { 1.0 };
            self.scroll = ((1.0 - frac) * history as f32).round() as usize;
            self.scrolled_at = now;
        }
        let thumb_top = track.top() + travel * top_frac;
        let thumb = Rect::from_min_size(Pos2::new(track.left(), thumb_top), Vec2::new(track.width(), thumb_h));
        let painter = ui.painter_at(rect);
        let hot = resp.dragged() || resp.hovered();
        painter.rect_filled(track, CornerRadius::same(3), theme::text_faint().gamma_multiply(0.12 * alpha));
        let fill = if hot { theme::text_dim() } else { theme::text_faint() };
        painter.rect_filled(thumb, CornerRadius::same(3), fill.gamma_multiply(0.85 * alpha));
    }

    /// "N lines up" pill; returns true when clicked (jump back to the live screen).
    fn scroll_pill(&self, ui: &mut Ui, rect: Rect) -> bool {
        let font = FontId::proportional(11.5);
        let label = format!("{} line{} up", self.scroll, if self.scroll == 1 { "" } else { "s" });
        let galley = ui.painter().layout_no_wrap(label, font, theme::text());
        let size = Vec2::new(galley.size().x + 38.0, 24.0);
        let pill = Rect::from_min_size(Pos2::new(rect.right() - size.x - 16.0, rect.top() + 10.0), size);
        let resp = ui.interact(pill, ui.id().with("term_pill"), Sense::click()).on_hover_text("Back to the live screen");
        let painter = ui.painter_at(rect);
        let hover = ui.ctx().animate_bool_with_time(ui.id().with("term_pill_h"), resp.hovered(), theme::anim_time(0.12));
        let fill = theme::surface_3().lerp_to_gamma(theme::accent_soft(), hover * 0.6);
        painter.rect_filled(pill.translate(Vec2::new(0.0, 2.0)), CornerRadius::same(12), theme::shadow().gamma_multiply(0.35));
        painter.rect(pill, CornerRadius::same(12), fill, Stroke::new(1.0, theme::border()), egui::StrokeKind::Inside);
        // Down chevron: a click returns to the bottom.
        let c = Pos2::new(pill.left() + 15.0, pill.center().y);
        let stroke = Stroke::new(1.6, theme::accent());
        painter.line_segment([c + Vec2::new(-4.0, -2.0), c + Vec2::new(0.0, 2.0)], stroke);
        painter.line_segment([c + Vec2::new(0.0, 2.0), c + Vec2::new(4.0, -2.0)], stroke);
        painter.galley(Pos2::new(pill.left() + 26.0, pill.center().y - galley.size().y * 0.5), galley, theme::text());
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp.clicked()
    }

    fn apply_resize(&mut self, rows: u16, cols: u16) {
        if self.size == (rows, cols) {
            return;
        }
        self.size = (rows, cols);
        if let Ok(mut s) = self.handle.shared.lock() {
            s.parser.screen_mut().set_size(rows, cols);
        }
        self.handle.resize(rows, cols);
    }

    fn set_font_size(&mut self, size: f32, persist_now: bool) {
        let size = size.clamp(TerminalSettings::FONT_MIN, TerminalSettings::FONT_MAX);
        if persist_now {
            let _ = settings::update(|s| s.terminal.font_size = size);
            self.font_dirty_at = None;
        } else {
            settings::preview(|s| s.terminal.font_size = size);
        }
    }

    fn persist_font_when_idle(&mut self, now: f64) {
        if let Some(at) = self.font_dirty_at
            && now - at > FONT_SAVE_DELAY
        {
            let _ = settings::save();
            self.font_dirty_at = None;
        }
    }

    fn handle_mouse(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        to_cell: impl Fn(Pos2) -> Cell,
        cell_h: f32,
        (alt_screen, app_cursor): (bool, bool),
        out: &mut ViewOutput,
    ) {
        if !alt_screen
            && response.secondary_clicked()
            && let Some(pos) = response.interact_pointer_pos()
            && self.cache.links.on_secondary_click(pos)
        {
            return;
        }
        if response.drag_started() {
            if let Some(p) = response.interact_pointer_pos() {
                self.selection = Some((to_cell(p), to_cell(p)));
            }
        } else if response.dragged() {
            if let (Some(p), Some((start, _))) = (response.interact_pointer_pos(), self.selection) {
                self.selection = Some((start, to_cell(p)));
            }
        } else if response.clicked() {
            let ctrl = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
            if !alt_screen && let Some(p) = response.interact_pointer_pos() {
                let (intercepted, action) = self.cache.links.on_click(p, ctrl, ui.ctx());
                if intercepted {
                    self.selection = None;
                    out.link_action(action);
                    return;
                }
            }
            self.selection = None;
        }

        if !response.hovered() {
            return;
        }
        let (wheel, zoom, ctrl, now) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta(), i.modifiers.ctrl, i.time));
        if ctrl && zoom != 1.0 {
            let current = settings::snapshot().terminal.font_size();
            self.set_font_size(current * zoom, false);
            self.font_dirty_at = Some(now);
            return;
        }
        if wheel == 0.0 || ctrl {
            return;
        }
        // Accumulate smooth deltas into whole lines.
        self.wheel_acc += wheel;
        let lines = (self.wheel_acc / cell_h).trunc();
        if lines == 0.0 {
            return;
        }
        self.wheel_acc -= lines * cell_h;
        let n = lines.abs() as usize;
        if alt_screen {
            // Full-screen apps (less, vim) get arrow keys instead of scrollback.
            let key = if lines > 0.0 { egui::Key::ArrowUp } else { egui::Key::ArrowDown };
            let seq = keys::encode(key, egui::Modifiers::NONE, app_cursor).unwrap_or_default();
            for _ in 0..n {
                out.input.extend_from_slice(&seq);
            }
        } else if lines > 0.0 {
            self.scroll += n;
        } else {
            self.scroll = self.scroll.saturating_sub(n);
        }
    }

    fn handle_keys(&mut self, ui: &Ui, app_cursor: bool, bracketed: bool, out: &mut ViewOutput) {
        let (events, mods) = ui.input(|i| (i.events.clone(), i.modifiers));
        for event in events {
            match event {
                Event::Text(t) => {
                    if !mods.ctrl && !mods.alt {
                        self.mark_prompt_start();
                        self.cache.typed_line.push_str(&t);
                    }
                    out.input.extend(keys::encode_text(&t, mods.alt && !mods.ctrl));
                }
                Event::Paste(t) => {
                    if !self.cache.paste_guard.explicit(ui.ctx().cumulative_frame_nr()) {
                        continue;
                    }
                    let files = super::os_input::clipboard_files();
                    if self.images_enabled && files.iter().any(|p| super::attachments::is_image(p)) {
                        let (images, other): (Vec<_>, Vec<_>) = files.into_iter().partition(|p| super::attachments::is_image(p));
                        self.cache.attachments.add(images, ui.ctx());
                        if !other.is_empty() {
                            out.input.extend(keys::encode_paste(&super::os_input::format_paths(&other), bracketed));
                        }
                        continue;
                    }
                    if self.images_enabled && super::os_input::clipboard_has_image() && self.cache.attachments.paste_image(ui.ctx()) {
                        continue;
                    }
                    self.mark_prompt_start();
                    if let Some(last) = t.rsplit(['\n', '\r']).next() {
                        if t.contains(['\n', '\r']) {
                            self.cache.typed_line.clear();
                        }
                        self.cache.typed_line.push_str(last);
                    }
                    out.input.extend(keys::encode_paste(&t, bracketed));
                }
                Event::Copy => match self.selected_text() {
                    Some(text) if !text.is_empty() => {
                        ui.ctx().copy_text(text);
                        self.selection = None;
                    }
                    _ => {
                        self.prompt_col = None;
                        self.cache.typed_line.clear();
                        out.input.push(0x03);
                    }
                },
                Event::Cut => out.input.push(0x18),
                Event::Key { key, pressed: true, modifiers, .. } => {
                    if self.images_enabled && key == egui::Key::Enter && !modifiers.shift && self.cache.attachments.is_pending() {
                        self.cache.attachments.notice = Some("Preparing images… Press Enter once the attachment is ready.".into());
                        continue;
                    }
                    if let Some(up) = super::blocks::step_key(key, modifiers).filter(|_| self.cache.blocks.active()) {
                        self.block_request(super::blocks::Request::Step(up), ui.ctx(), out);
                        continue;
                    }
                    if modifiers.shift && matches!(key, egui::Key::PageUp | egui::Key::PageDown) {
                        let page = self.size.0.max(2) as usize - 1;
                        self.scroll = if key == egui::Key::PageUp { self.scroll + page } else { self.scroll.saturating_sub(page) };
                        continue;
                    }
                    if matches!(key, egui::Key::ArrowUp | egui::Key::ArrowDown | egui::Key::Tab) {
                        self.mark_prompt_start();
                    } else if key == egui::Key::Enter && modifiers.shift && !modifiers.ctrl && !modifiers.alt {
                        self.cache.typed_line.push('\n');
                    } else if key == egui::Key::Backspace && !modifiers.ctrl && !modifiers.alt {
                        self.cache.typed_line.pop();
                    } else if (modifiers.ctrl || modifiers.command) && matches!(key, egui::Key::C | egui::Key::U) {
                        self.prompt_col = None;
                        self.cache.typed_line.clear();
                    } else if key == egui::Key::Enter && !modifiers.shift && !modifiers.ctrl && !modifiers.alt && !modifiers.command {
                        if let Some(files) = self.take_nano_command().filter(|_| self.intercept_nano) {
                            // Cancel the typed command line in the remote shell and open the file(s) in Tether's editor.
                            out.input.push(0x03);
                            out.open_files.extend(files);
                            continue;
                        }
                    }
                    let font = settings::snapshot().terminal.font_size();
                    let is = |a: Action| !keybindings::is_recording() && keybindings::binding(a).is_some_and(|b| b.matches(key, modifiers));
                    if is(Action::FontBigger) {
                        self.set_font_size(font + 1.0, true);
                    } else if is(Action::FontSmaller) {
                        self.set_font_size(font - 1.0, true);
                    } else if is(Action::FontReset) {
                        self.set_font_size(TerminalSettings::default().font_size, true);
                    } else if let Some(bytes) = keys::encode(key, modifiers, app_cursor) {
                        out.input.extend(bytes);
                    }
                }
                _ => {}
            }
        }
    }

    /// Ctrl+V with no text on the clipboard: egui-winit drops the key press entirely, so
    /// it is detected from the physical key state / the V release instead. Files copied in
    /// Explorer paste as paths; screenshots become image attachments.
    fn paste_without_text(&mut self, ui: &Ui, bracketed: bool, out: &mut ViewOutput) {
        let (had_paste, released_v) = ui.input(|i| {
            let paste = i.events.iter().any(|e| matches!(e, Event::Paste(_)));
            let release = i.events.iter().any(|e| {
                matches!(e, Event::Key { key: egui::Key::V, pressed: false, modifiers, .. } if (modifiers.ctrl || modifiers.command) && !modifiers.shift && !modifiers.alt)
            });
            (paste, release)
        });
        let down = super::os_input::ctrl_v_down();
        if self.cache.paste_guard.fallback(ui.ctx().cumulative_frame_nr(), down, released_v, had_paste) {
            self.paste_clipboard_non_text(ui.ctx(), bracketed, out);
        }
    }

    fn paste_clipboard_non_text(&mut self, ctx: &egui::Context, bracketed: bool, out: &mut ViewOutput) {
        let files = super::os_input::clipboard_files();
        if !files.is_empty() {
            let (images, files): (Vec<_>, Vec<_>) = files.into_iter().partition(|p| self.images_enabled && super::attachments::is_image(p));
            if !images.is_empty() {
                self.cache.attachments.add(images, ctx);
            }
            if !files.is_empty() {
                self.mark_prompt_start();
                out.input.extend(keys::encode_paste(&super::os_input::format_paths(&files), bracketed));
            }
            return;
        }
        if self.images_enabled && super::os_input::clipboard_has_image() && !super::os_input::clipboard_has_text() {
            self.cache.attachments.paste_image(ctx);
        }
    }

    /// Files dragged from Explorer onto this terminal: their paths are typed at the prompt
    /// (bracketed paste, so Claude Code attaches dropped images). True when something landed.
    fn file_drop(&mut self, ui: &Ui, rect: Rect, bracketed: bool, out: &mut ViewOutput) -> bool {
        let (hovering, dropped): (bool, Vec<std::path::PathBuf>) = ui.input(|i| {
            (!i.raw.hovered_files.is_empty(), i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect())
        });
        if !hovering && dropped.is_empty() {
            return false;
        }
        let visible = rect.intersect(ui.clip_rect());
        let over = super::os_input::drop_pointer_pos(ui.ctx()).is_some_and(|p| visible.contains(p));
        let glow = ui.ctx().animate_bool_with_time(ui.id().with("file_drop_glow"), hovering && over, 0.15);
        if glow > 0.0 {
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::accent().gamma_multiply(0.08 * glow));
            painter.rect_stroke(
                rect.shrink(1.0),
                CornerRadius::same(theme::RADIUS),
                Stroke::new(1.5, theme::accent().gamma_multiply(glow)),
                egui::StrokeKind::Inside,
            );
            painter.text(rect.center(), Align2::CENTER_CENTER, "Drop to insert path", FontId::proportional(14.0), theme::text().gamma_multiply(glow));
            ui.ctx().request_repaint();
        }
        if dropped.is_empty() || !over {
            return false;
        }
        let frame = ui.ctx().cumulative_frame_nr();
        if self.cache.last_drop_frame == Some(frame) {
            return false;
        }
        self.cache.last_drop_frame = Some(frame);
        self.mark_prompt_start();
        let (images, files): (Vec<_>, Vec<_>) = dropped.into_iter().partition(|p| self.images_enabled && super::attachments::is_image(p));
        self.cache.attachments.add(images, ui.ctx());
        out.input.extend(keys::encode_paste(&super::os_input::format_paths(&files), bracketed));
        true
    }

    /// Scrolls back `lines` rows (screenshot tests).
    #[cfg(test)]
    pub(crate) fn set_scroll(&mut self, lines: usize) {
        self.scroll = lines;
    }

    /// Previous / next prompt from outside the view (command palette). False when the
    /// terminal has no command blocks.
    pub fn step_prompt(&mut self, up: bool, ctx: &egui::Context) -> bool {
        if !self.cache.blocks.active() {
            return false;
        }
        let handle = self.handle.clone();
        if let Some(bytes) = self.cache.blocks.apply(super::blocks::Request::Step(up), &handle, ctx, &mut self.scroll) {
            handle.send(bytes);
        }
        ctx.request_repaint();
        true
    }

    /// A block mark, menu or Ctrl+Shift+Up/Down asked for something (see `ui::blocks`).
    fn block_request(&mut self, req: super::blocks::Request, ctx: &egui::Context, out: &mut ViewOutput) {
        let handle = self.handle.clone();
        if let Some(bytes) = self.cache.blocks.apply(req, &handle, ctx, &mut self.scroll) {
            out.input.extend(bytes);
        }
        ctx.request_repaint();
    }

    fn mark_prompt_start(&mut self) {
        if self.prompt_col.is_some() {
            return;
        }
        if let Ok(shared) = self.handle.shared.lock() {
            let screen = shared.parser.screen();
            if !screen.alternate_screen() {
                self.prompt_col = Some(screen.cursor_position());
            }
        }
    }

    /// Reads the command on the current shell prompt line and checks whether it is a
    /// `nano <file…>` invocation that should open in Tether's built-in editor.
    fn take_nano_command(&mut self) -> Option<Vec<String>> {
        let prompt = self.prompt_col.take();
        let typed = std::mem::take(&mut self.cache.typed_line);
        if typed.contains(['\r', '\n']) {
            return None;
        }
        let shared = self.handle.shared.lock().ok()?;
        let screen = shared.parser.screen();
        if screen.alternate_screen() {
            return None;
        }
        let (cr, cc) = screen.cursor_position();
        let (_, cols) = screen.size();
        if let Some((pr, pc)) = prompt
            && (pr, pc) <= (cr, cols)
        {
            let full = screen.contents_between(pr, pc, cr, cols);
            let before = screen.contents_between(pr, pc, cr, cc.max(pc));
            let from_prompt = trim_after_cursor(&full, &before);
            if let Some(files) = parse_nano_command(&from_prompt) {
                return Some(files);
            }
        }
        let full_row = screen.contents_between(cr, 0, cr, cols);
        let before_cursor = screen.contents_between(cr, 0, cr, cc);
        let row_line = trim_after_cursor(&full_row, &before_cursor);
        if let Some(cmd) = strip_shell_prompt(&row_line)
            && let Some(files) = parse_nano_command(cmd)
        {
            return Some(files);
        }
        parse_nano_command(&typed)
    }

    fn selected_text(&self) -> Option<String> {
        let (a, b) = self.selection?;
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        if a == b {
            return None;
        }
        let shared = self.handle.shared.lock().ok()?;
        let text = shared.parser.screen().contents_between(a.row, a.col, b.row, b.col);
        Some(text.trim_end().to_string())
    }
}

/// Keeps the command text up to (and including any single-spaced word after) the cursor,
/// stopping before any right-aligned prompt (`RPROMPT`) separated by 2+ spaces.
fn trim_after_cursor(full: &str, before_cursor: &str) -> String {
    let split_at = before_cursor.len().min(full.len());
    let (head, tail) = full.split_at(split_at);
    let tail_keep = match tail.find("  ") {
        Some(i) => &tail[..i],
        None => tail,
    };
    format!("{head}{tail_keep}").trim().to_string()
}

/// Strips a shell prompt prefix (`user@host:~$ `, `root@host:# `, `➜  dir `, etc.) from a line.
pub fn strip_shell_prompt(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if parse_nano_command(trimmed).is_some() {
        return Some(trimmed);
    }
    const MARKERS: [&str; 7] = ["$ ", "# ", "% ", "> ", "❯ ", "» ", "λ "];
    let mut best_end = None;
    for m in MARKERS {
        if let Some(pos) = trimmed.rfind(m) {
            let end = pos + m.len();
            if best_end.is_none_or(|b| end > b) {
                best_end = Some(end);
            }
        }
    }
    if let Some(end) = best_end {
        return Some(trimmed[end..].trim_start());
    }
    if trimmed.starts_with('➜') {
        for (idx, _) in trimmed.match_indices("nano ") {
            if idx == 0 || trimmed.as_bytes()[idx - 1].is_ascii_whitespace() {
                return Some(&trimmed[idx..]);
            }
        }
    }
    None
}

/// Parses a `nano <files…>` command into its file arguments, or `None` when the command
/// is not `nano`, has no file targets, or uses shell operators/redirections.
pub fn parse_nano_command(cmd: &str) -> Option<Vec<String>> {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return None;
    }
    let words = split_shell_words(cmd)?;
    let prog = words.first()?.as_str();
    if !matches!(prog, "nano" | "/bin/nano" | "/usr/bin/nano" | "tether") {
        return None;
    }
    let mut files = Vec::new();
    let mut args = words[1..].iter();
    let mut options_done = false;
    while let Some(arg) = args.next() {
        if !options_done {
            if arg == "--" {
                options_done = true;
                continue;
            }
            if matches!(arg.as_str(), "-h" | "--help" | "-V" | "--version") {
                return None;
            }
            if arg.starts_with('+') && arg[1..].chars().all(|c| c.is_ascii_digit() || c == ',' || c == ':') {
                continue;
            }
            if arg.starts_with('-') && arg.len() > 1 {
                if matches!(
                    arg.as_str(),
                    "-C" | "--backupdir"
                        | "-o"
                        | "--operatingdir"
                        | "-r"
                        | "--fill"
                        | "-s"
                        | "--speller"
                        | "-T"
                        | "--tabsize"
                        | "-Q"
                        | "--quotestr"
                        | "-Y"
                        | "--syntax"
                ) {
                    let _ = args.next();
                }
                continue;
            }
        }
        if !arg.is_empty() {
            files.push(arg.clone());
        }
    }
    (!files.is_empty()).then_some(files)
}

fn split_shell_words(cmd: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '|' | '&' | ';' | '>' | '<' | '`' | '$' => return None,
            ' ' | '\t' => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                for sc in chars.by_ref() {
                    if sc == '\'' {
                        break;
                    }
                    cur.push(sc);
                }
            }
            '"' => {
                in_word = true;
                while let Some(dc) = chars.next() {
                    if dc == '"' {
                        break;
                    }
                    if dc == '\\'
                        && let Some(&next) = chars.peek()
                        && matches!(next, '"' | '\\' | '$' | '`')
                    {
                        cur.push(chars.next().unwrap());
                        continue;
                    }
                    if matches!(dc, '$' | '`') {
                        return None;
                    }
                    cur.push(dc);
                }
            }
            '\\' => {
                in_word = true;
                cur.push(chars.next()?);
            }
            _ => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Some(words)
}

/// Fraction of the track above the thumb and the thumb's share of the track,
/// for `scroll` lines up out of `history` with `rows` visible.
fn thumb_span(scroll: usize, history: usize, rows: usize) -> (f32, f32) {
    let total = (history + rows).max(1) as f32;
    let len = (rows as f32 / total).clamp(0.0, 1.0);
    let top = if history == 0 { 1.0 } else { 1.0 - (scroll.min(history) as f32 / history as f32) };
    (top, len)
}

/// Paints the cursor in one of the three styles. Shared with the settings preview.
pub fn paint_cursor(painter: &egui::Painter, cell: Rect, text_dy: f32, style: CursorStyle, focused: bool, glyph: Option<&str>, font: &FontId) {
    let colour = theme::term_cursor();
    match (style, focused) {
        (CursorStyle::Block, true) => {
            painter.rect_filled(cell, CornerRadius::same(2), colour.gamma_multiply(0.9));
            if let Some(g) = glyph {
                painter.text(cell.min + Vec2::new(0.0, text_dy), Align2::LEFT_TOP, g, font.clone(), theme::term_bg());
            }
        }
        (CursorStyle::Block, false) => {
            let stroke = Stroke::new(1.0, colour.gamma_multiply(0.6));
            painter.rect_stroke(cell, CornerRadius::same(2), stroke, egui::StrokeKind::Inside);
        }
        (CursorStyle::Bar, _) => {
            let w = (cell.width() * 0.14).clamp(1.5, 3.0);
            let bar = Rect::from_min_size(cell.min, Vec2::new(w, cell.height()));
            painter.rect_filled(bar, CornerRadius::same(1), colour.gamma_multiply(if focused { 1.0 } else { 0.5 }));
        }
        (CursorStyle::Underline, _) => {
            let h = (cell.height() * 0.1).clamp(1.5, 3.0);
            let line = Rect::from_min_max(Pos2::new(cell.left(), cell.bottom() - h), cell.max);
            painter.rect_filled(line, CornerRadius::same(1), colour.gamma_multiply(if focused { 1.0 } else { 0.5 }));
        }
    }
}

/// Selected `[start, end)` columns of `row` for an ordered selection, if any.
fn selection_cols(sel: Option<(Cell, Cell)>, row: u16, cols: u16) -> Option<(u16, u16)> {
    let (a, b) = sel?;
    if a == b || row < a.row || row > b.row {
        return None;
    }
    let start = if row == a.row { a.col } else { 0 };
    let end = if row == b.row { b.col } else { cols };
    (start < end).then_some((start, end))
}

/// Connecting / failed / closed overlays. Returns true when "Reconnect" is clicked.
fn state_overlay(ui: &mut Ui, rect: Rect, state: &LinkState) -> bool {
    let id = ui.id().with("overlay");
    // Connecting has its own full-pane screen (`connecting_screen`).
    let visible = matches!(state, LinkState::Failed(_) | LinkState::Closed(_));
    let t = ui.ctx().animate_bool_with_time(id, visible, theme::anim_time(0.25));
    if t <= 0.0 {
        return false;
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::bg().gamma_multiply(0.72 * t));
    let (title, detail, color, can_retry) = match state {
        LinkState::Connecting => ("Connecting…", String::new(), theme::accent(), false),
        LinkState::Failed(e) => ("Connection failed", e.clone(), theme::danger(), true),
        LinkState::Closed(r) => ("Session ended", r.clone(), theme::text_dim(), true),
        LinkState::Connected => ("", String::new(), theme::accent(), false),
    };
    let card = Rect::from_center_size(rect.center() + Vec2::new(0.0, 8.0 * (1.0 - t)), Vec2::new(360.0_f32.min(rect.width() - 24.0), 132.0));
    let mut clicked = false;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(card));
    child.set_opacity(t);
    egui::Frame::new()
        .fill(theme::surface_2())
        .stroke(Stroke::new(1.0, theme::border()))
        .corner_radius(12)
        .inner_margin(16)
        .shadow(egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: theme::shadow() })
        .show(&mut child, |ui| {
            ui.set_width(card.width() - 32.0);
            ui.horizontal(|ui| {
                if matches!(state, LinkState::Connecting) {
                    ui.add(egui::Spinner::new().size(16.0).color(color));
                } else {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 5.0, color);
                }
                ui.label(egui::RichText::new(title).strong().size(15.0));
            });
            if !detail.is_empty() {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(detail).color(theme::text_dim()).size(12.0));
            }
            if can_retry {
                ui.add_space(8.0);
                clicked = ui.add(egui::Button::new("Reconnect").fill(theme::accent_soft())).clicked();
            }
        });
    clicked
}

#[cfg(test)]
#[path = "terminal_bench.rs"]
mod bench;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_button_follows_foreground_agent_and_releases_its_space() {
        use egui_kittest::kittest::Queryable;
        let enabled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let state = enabled.clone();
        let (handle, _rx) = TerminalHandle::new(24, 80);
        handle.shared.lock().unwrap().state = LinkState::Connected;
        let mut view = TerminalView::new(handle);
        let mut h = egui_kittest::Harness::builder().with_size(Vec2::new(800.0, 500.0)).build_ui(move |ui| {
            view.images_enabled = state.load(std::sync::atomic::Ordering::Relaxed);
            view.ui(ui, &Profile::new());
        });
        h.run_steps(3);
        assert!(h.query_by_label("+ Image").is_none());
        let height = h.get_by_label("Terminal").rect().height();
        enabled.store(true, std::sync::atomic::Ordering::Relaxed);
        h.run_steps(3);
        h.get_by_label("+ Image");
        assert_eq!(h.get_by_label("Terminal").rect().height(), height - 34.0);
        enabled.store(false, std::sync::atomic::Ordering::Relaxed);
        h.run_steps(3);
        assert!(h.query_by_label("+ Image").is_none());
        assert_eq!(h.get_by_label("Terminal").rect().height(), height);
    }

    #[test]
    fn clicking_the_ssh_prompt_directory_emits_sftp_not_terminal_input() {
        use egui_kittest::kittest::Queryable;
        let prompt = "prisma@na-prisma-02:~/servers/practice-servers/na-practice-01$";
        let (handle, _rx) = TerminalHandle::new(24, 100);
        {
            let mut s = handle.shared.lock().unwrap();
            s.state = LinkState::Connected;
            s.parser.process(prompt.as_bytes());
        }
        let mut view = TerminalView::new(handle);
        let actions = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let output = actions.clone();
        let pointer = std::sync::Arc::new(std::sync::Mutex::new(Pos2::ZERO));
        let position = pointer.clone();
        let mut h = egui_kittest::Harness::builder().with_size(Vec2::new(1000.0, 500.0)).build_ui(move |ui| {
            let settings = settings::snapshot();
            let cfg = &settings.terminal;
            let font = FontId::new(cfg.font_size(), FontFamily::Monospace);
            let m = metrics(ui.ctx(), &font, cfg.line_height());
            let inner = ui.available_rect_before_wrap().shrink(cfg.padding());
            *position.lock().unwrap() = inner.min + Vec2::new((prompt.find("servers").unwrap() as f32 + 2.5) * m.cell_w, m.cell_h * 0.5);
            let out = view.ui(ui, &Profile::new());
            assert!(out.input.is_empty());
            output.lock().unwrap().extend(out.open_folders);
        });
        h.run_steps(3);
        let pos = *pointer.lock().unwrap();
        h.event(Event::PointerMoved(pos));
        h.run_steps(2);
        for pressed in [true, false] {
            h.event(Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
            h.run_steps(2);
        }
        assert_eq!(&*actions.lock().unwrap(), &["~prisma/servers/practice-servers/na-practice-01"]);
        for pressed in [true, false] {
            h.event(Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE });
            h.run_steps(2);
        }
        h.get_by_label("📁  Open in SFTP").click();
        h.run_steps(3);
        assert_eq!(actions.lock().unwrap().len(), 2);
    }

    #[test]
    fn first_image_drop_is_sent_once_even_when_layout_repeats() {
        use crate::ssh::terminal::TermCmd;
        #[derive(Debug)]
        struct FileDrop(std::path::PathBuf);
        impl egui::DroppedFile for FileDrop {
            fn path(&self) -> &std::path::Path {
                &self.0
            }
            fn bytes(&self) -> Result<Vec<u8>, String> {
                std::fs::read(&self.0).map_err(|e| e.to_string())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        image::RgbaImage::new(8, 8).save(&path).unwrap();
        let (handle, mut rx) = TerminalHandle::new(24, 80);
        {
            let mut s = handle.shared.lock().unwrap();
            s.state = LinkState::Connected;
            s.parser.process(b"\x1b[?2004h");
        }
        let mut view = TerminalView::new(handle);
        view.images_enabled = true;
        let ctx = egui::Context::default();
        let mut sent = Vec::new();
        for gesture in 0..2 {
            let mut repeat_layout = true;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 400.0))),
                events: vec![Event::PointerMoved(Pos2::new(100.0, 100.0))],
                dropped_files: vec![std::sync::Arc::new(FileDrop(path.clone()))],
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                let out = view.ui(ui, &Profile::new());
                assert!(out.input.is_empty(), "image-only drops must not send an empty bracketed paste");
                if std::mem::take(&mut repeat_layout) {
                    ctx.request_discard("exercise repeated input during attachment layout");
                }
            });
            output.textures_delta.clear();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while view.cache.attachments.is_pending() {
                let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 400.0))), ..Default::default() };
                let mut output = ctx.run_ui(input, |ui| {
                    view.ui(ui, &Profile::new());
                });
                output.textures_delta.clear();
                assert!(std::time::Instant::now() < deadline, "attachment worker did not finish");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            while let Ok(cmd) = rx.try_recv() {
                if let TermCmd::Input(bytes) = cmd {
                    sent.push(bytes);
                }
            }
            assert_eq!(sent.len(), gesture + 1, "each intentional drop must send exactly one path");
            let text = String::from_utf8_lossy(sent.last().unwrap());
            assert_eq!(text.matches(".png").count(), 1);
            assert!(text.starts_with("\x1b[200~") && text.ends_with("\x1b[201~"));
        }
    }

    #[test]
    fn selection_spans_rows() {
        let sel = Some((Cell { row: 1, col: 5 }, Cell { row: 3, col: 2 }));
        assert_eq!(selection_cols(sel, 0, 80), None);
        assert_eq!(selection_cols(sel, 1, 80), Some((5, 80)));
        assert_eq!(selection_cols(sel, 2, 80), Some((0, 80)));
        assert_eq!(selection_cols(sel, 3, 80), Some((0, 2)));
        assert_eq!(selection_cols(sel, 4, 80), None);
        assert_eq!(selection_cols(None, 1, 80), None);
        let empty = Some((Cell { row: 1, col: 5 }, Cell { row: 1, col: 5 }));
        assert_eq!(selection_cols(empty, 1, 80), None);
    }

    #[test]
    fn renders_headless_with_history() {
        let (handle, _rx) = TerminalHandle::new(24, 80);
        if let Ok(mut s) = handle.shared.lock() {
            s.state = LinkState::Connected;
            for i in 0..200 {
                s.parser.process(format!("\x1b[3{}mline {i}\x1b[0m\r\n", i % 8).as_bytes());
            }
            s.parser.process(b"\x07");
        }
        let mut view = TerminalView::new(handle);
        view.scroll = 50;
        let ctx = egui::Context::default();
        for i in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0))),
                time: Some(i as f64 * 0.1),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                view.ui(ui, &Profile::new());
            });
            out.textures_delta.clear();
        }
        assert_eq!(view.scroll, 50);
        assert_eq!(view.last_bell, 1);
        assert!(view.size.0 > 10 && view.size.1 > 40);
    }

    #[test]
    fn scrollbar_thumb_tracks_offset() {
        assert_eq!(thumb_span(0, 100, 20), (1.0, 20.0 / 120.0));
        assert_eq!(thumb_span(100, 100, 20).0, 0.0);
        assert_eq!(thumb_span(50, 100, 20).0, 0.5);
        assert_eq!(thumb_span(500, 100, 20).0, 0.0);
        assert_eq!(thumb_span(0, 0, 20), (1.0, 1.0));
    }

    #[test]
    fn parses_nano_commands_and_strips_prompts() {
        assert_eq!(parse_nano_command("nano foo.txt"), Some(vec!["foo.txt".to_string()]));
        assert_eq!(parse_nano_command("nano -l +10,2 'my file.rs' b.toml"), Some(vec!["my file.rs".to_string(), "b.toml".to_string()]));
        assert_eq!(parse_nano_command("nano -T 4 config.yaml"), Some(vec!["config.yaml".to_string()]));
        assert_eq!(parse_nano_command("nano"), None);
        assert_eq!(parse_nano_command("nano --version"), None);
        assert_eq!(parse_nano_command("nano foo.txt | cat"), None);
        assert_eq!(strip_shell_prompt("user@host:~/app$ nano src/main.rs"), Some("nano src/main.rs"));
        assert_eq!(strip_shell_prompt("root@box:/etc# nano nginx.conf"), Some("nano nginx.conf"));
        assert_eq!(strip_shell_prompt("➜  project git:(main) nano Cargo.toml"), Some("nano Cargo.toml"));
    }
}
