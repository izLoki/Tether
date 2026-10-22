//! Automatic link and file-path detection in the terminal grid (Warp-style).
//!
//! Only the logical line under the mouse pointer (plus any line holding an open link
//! menu) is scanned, so idle or streaming terminals pay nothing per frame. Candidate
//! tokens are resolved against the terminal's working directory (OSC 7 / agent cwd /
//! initial shell cwd) with a small per-directory existence cache.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, CornerRadius, FontFamily, FontId, Id, Order, Pos2, Rect, Stroke, Ui, Vec2};

use super::terminal_view::Metrics;
use crate::theme;

const CACHE_TTL: Duration = Duration::from_secs(3);
const MAX_CACHE_ENTRIES: usize = 256;

/// A contiguous span of cells on one visible screen row `[start_col, end_col)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowSpan {
    pub row: u16,
    pub start_col: u16,
    pub end_col: u16,
}

/// What a detected terminal link points to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// `http://` or `https://` URL.
    Url(String),
    /// A local file or folder (resolved absolute path, and whether it is a directory).
    LocalPath { resolved: PathBuf, is_dir: bool, line: Option<u32>, col: Option<u32> },
    /// A file path in a remote SSH session (resolved against remote cwd when relative).
    RemotePath { path: String, line: Option<u32> },
}

/// A detected clickable link covering one or more row spans (when wrapped across lines).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedLink {
    /// Text as printed in the terminal (without `:line:col` suffix for paths).
    pub raw: String,
    pub target: LinkTarget,
    pub spans: Vec<RowSpan>,
}

impl DetectedLink {
    pub fn contains(&self, row: u16, col: u16) -> bool {
        self.spans.iter().any(|s| s.row == row && col >= s.start_col && col < s.end_col)
    }

    /// Bounding rect of the first span in screen coordinates, for anchoring the popup.
    pub fn anchor_rect(&self, inner: Rect, m: Metrics) -> Rect {
        let Some(first) = self.spans.first() else {
            return Rect::from_min_size(inner.min, Vec2::ZERO);
        };
        let min = Pos2::new(inner.left() + first.start_col as f32 * m.cell_w, inner.top() + first.row as f32 * m.cell_h);
        let size = Vec2::new((first.end_col.saturating_sub(first.start_col)).max(1) as f32 * m.cell_w, m.cell_h);
        Rect::from_min_size(min, size)
    }
}

/// Action chosen by the user from a link popup (or Ctrl+click).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkAction {
    /// Open this remote path in Tether's built-in editor.
    OpenRemote(String),
    /// Browse the remote directory (or the containing folder for a file).
    OpenSftp(String),
}

/// Cached existence lookup for `(cwd, raw_path)`.
#[derive(Default)]
pub struct PathProbeCache {
    cwd: String,
    entries: HashMap<String, (Option<(PathBuf, bool)>, Instant)>,
}

impl PathProbeCache {
    fn resolve(&mut self, cwd: Option<&str>, raw: &str) -> Option<(PathBuf, bool)> {
        let cwd_str = cwd.unwrap_or_default();
        if self.cwd != cwd_str {
            self.cwd = cwd_str.to_string();
            self.entries.clear();
        }
        let now = Instant::now();
        if let Some((cached, at)) = self.entries.get(raw)
            && now.saturating_duration_since(*at) < CACHE_TTL
        {
            return cached.clone();
        }
        if self.entries.len() >= MAX_CACHE_ENTRIES {
            self.entries.clear();
        }
        let res = resolve_local_path(cwd, raw);
        self.entries.insert(raw.to_string(), (res.clone(), now));
        res
    }
}

/// Per-terminal link detector and popup state.
#[derive(Default)]
pub struct LinksState {
    /// Key of the last hover query: `(generation, scroll, row, col, is_local)`.
    hover_key: Option<(u64, usize, u16, u16, bool)>,
    hovered: Option<DetectedLink>,
    /// Link whose action menu is currently open, and the screen pos it was clicked at.
    active_menu: Option<(DetectedLink, Pos2)>,
    probe_cache: PathProbeCache,
}

impl LinksState {
    pub fn has_open_menu(&self) -> bool {
        self.active_menu.is_some()
    }

    pub fn close_menu(&mut self) {
        self.active_menu = None;
    }

    /// Updates the hovered link for `(hover_cell)` under the terminal lock.
    pub fn sync(&mut self, screen: &vt100::Screen, generation: u64, scroll: usize, hover_cell: Option<(u16, u16)>, cwd: Option<&str>, is_local: bool) {
        let Some((row, col)) = hover_cell else {
            self.hover_key = None;
            self.hovered = None;
            return;
        };
        let key = (generation, scroll, row, col, is_local);
        if self.hover_key == Some(key) {
            return;
        }
        if let Some(prev) = &self.hovered
            && self.hover_key.is_some_and(|(g, s, _, _, l)| g == generation && s == scroll && l == is_local)
            && prev.contains(row, col)
        {
            self.hover_key = Some(key);
            return;
        }
        self.hover_key = Some(key);
        self.hovered = detect_at(screen, row, col, cwd, is_local, &mut self.probe_cache);
    }

    /// True when the pointer is currently over a detected link.
    pub fn is_hovering_link(&self) -> bool {
        self.hovered.is_some()
    }

    /// Handles a click on the terminal; returns `true` if a link intercepted the click.
    pub fn on_click(&mut self, pos: Pos2, ctrl: bool, ctx: &egui::Context) -> (bool, Option<LinkAction>) {
        if let Some(link) = self.hovered.clone() {
            if !ctrl && let LinkTarget::RemotePath { path, .. } = &link.target {
                self.active_menu = None;
                return (true, Some(LinkAction::OpenSftp(path.clone())));
            }
            if ctrl {
                let action = execute_primary(&link.target, ctx);
                self.active_menu = None;
                return (true, action);
            }
            self.active_menu = Some((link, pos));
            return (true, None);
        }
        if self.active_menu.is_some() {
            self.active_menu = None;
        }
        (false, None)
    }

    pub fn on_secondary_click(&mut self, pos: Pos2) -> bool {
        self.active_menu = self.hovered.clone().map(|link| (link, pos));
        self.active_menu.is_some()
    }

    /// Paints the underline for the hovered (or menu-active) link and renders the popup
    /// menu when open.
    pub fn paint_and_menu(&mut self, ui: &mut Ui, painter: &egui::Painter, inner: Rect, m: Metrics, accent: Color32) -> Option<LinkAction> {
        let active_link = self.active_menu.as_ref().map(|(l, _)| l).or(self.hovered.as_ref());
        if let Some(link) = active_link {
            paint_link_spans(painter, inner, m, &link.spans, accent);
        }
        if self.hovered.is_some() && self.active_menu.is_none() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            if let Some(link) = &self.hovered {
                let tip = match &link.target {
                    LinkTarget::Url(u) => format!("{u}\nClick for options · Ctrl+click to open"),
                    LinkTarget::LocalPath { resolved, is_dir, .. } => {
                        let kind = if *is_dir { "Folder" } else { "File" };
                        format!("{kind}: {}\nClick for options · Ctrl+click to open", resolved.display())
                    }
                    LinkTarget::RemotePath { path, .. } => format!("Remote: {path}\nClick to open SFTP · Right-click for options · Ctrl+click to edit"),
                };
                egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), Id::new("term_link_tip"), egui::PopupAnchor::Pointer).gap(10.0).show(|ui| {
                    ui.label(egui::RichText::new(tip).size(11.5).color(theme::text()));
                });
            }
        }
        self.show_menu(ui, inner, m)
    }

    fn show_menu(&mut self, ui: &mut Ui, inner: Rect, m: Metrics) -> Option<LinkAction> {
        let (link, click_pos) = self.active_menu.clone()?;
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.active_menu = None;
            return None;
        }
        let anchor = link.anchor_rect(inner, m);
        let menu_pos = Pos2::new(click_pos.x.clamp(inner.left() + 4.0, (inner.right() - 230.0).max(inner.left() + 4.0)), anchor.bottom() + 4.0);
        let area_id = Id::new("term_link_popup_menu");
        let mut action = None;
        let mut close = false;
        let area_resp = egui::Area::new(area_id)
            .order(Order::Foreground)
            .fixed_pos(menu_pos)
            .constrain(true)
            .show(&ctx, |ui| {
                let frame = egui::Frame::popup(ui.style())
                    .fill(theme::surface_2())
                    .stroke(Stroke::new(1.0, theme::border()))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::same(6))
                    .shadow(egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: theme::shadow() });
                frame.show(ui, |ui| {
                    ui.set_min_width(210.0);
                    ui.set_max_width(340.0);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    match &link.target {
                        LinkTarget::LocalPath { resolved, is_dir, .. } => {
                            let full = resolved.display().to_string();
                            path_header(ui, &full);
                            ui.separator();
                            let open_label = if *is_dir { "📁  Open folder in Explorer" } else { "↗  Open file" };
                            if menu_btn(ui, open_label) {
                                let _ = crate::remote_edit::open_with_system(resolved);
                                close = true;
                            }
                            if !*is_dir && menu_btn(ui, "📂  Show in Explorer") {
                                let _ = reveal_in_explorer(resolved);
                                close = true;
                            }
                            if menu_btn(ui, "⧉  Copy path") {
                                ctx.copy_text(full.clone());
                                close = true;
                            }
                            if link.raw != full && menu_btn(ui, "⧉  Copy relative path") {
                                ctx.copy_text(link.raw.clone());
                                close = true;
                            }
                        }
                        LinkTarget::RemotePath { path, .. } => {
                            path_header(ui, path);
                            ui.separator();
                            if menu_btn(ui, "📁  Open in SFTP") {
                                action = Some(LinkAction::OpenSftp(path.clone()));
                                close = true;
                            }
                            if menu_btn(ui, "📝  Open in editor") {
                                action = Some(LinkAction::OpenRemote(path.clone()));
                                close = true;
                            }
                            if menu_btn(ui, "⧉  Copy path") {
                                ctx.copy_text(path.clone());
                                close = true;
                            }
                            if link.raw != *path && menu_btn(ui, "⧉  Copy printed path") {
                                ctx.copy_text(link.raw.clone());
                                close = true;
                            }
                        }
                        LinkTarget::Url(url) => {
                            path_header(ui, url);
                            ui.separator();
                            if menu_btn(ui, "🌐  Open in browser") {
                                let _ = open_url(url);
                                close = true;
                            }
                            if menu_btn(ui, "⧉  Copy link") {
                                ctx.copy_text(url.clone());
                                close = true;
                            }
                        }
                    }
                });
            })
            .response;

        // Close if the user clicked outside the menu.
        let clicked_outside = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer.interact_pos().is_some_and(|p| !area_resp.rect.contains(p) && !link.spans.iter().any(|s| span_rect(inner, m, *s).contains(p)))
        });
        if close || clicked_outside {
            self.active_menu = None;
        }
        action
    }
}

fn path_header(ui: &mut Ui, text: &str) {
    let shown = if text.chars().count() > 44 {
        let tail: String = text.chars().rev().take(41).collect::<Vec<_>>().into_iter().rev().collect();
        format!("…{tail}")
    } else {
        text.to_string()
    };
    ui.label(egui::RichText::new(shown).font(FontId::new(11.0, FontFamily::Monospace)).color(theme::text_faint()));
}

fn menu_btn(ui: &mut Ui, label: &str) -> bool {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        ui.painter().rect_filled(r, CornerRadius::same(5), theme::accent().gamma_multiply(0.16));
    }
    ui.painter().text(Pos2::new(r.left() + 8.0, r.center().y), Align2::LEFT_CENTER, label, FontId::proportional(12.5), theme::text());
    resp.clicked()
}

fn execute_primary(target: &LinkTarget, _ctx: &egui::Context) -> Option<LinkAction> {
    match target {
        LinkTarget::Url(url) => {
            let _ = open_url(url);
            None
        }
        LinkTarget::LocalPath { resolved, .. } => {
            let _ = crate::remote_edit::open_with_system(resolved);
            None
        }
        LinkTarget::RemotePath { path, .. } => Some(LinkAction::OpenRemote(path.clone())),
    }
}

fn span_rect(inner: Rect, m: Metrics, span: RowSpan) -> Rect {
    Rect::from_min_size(
        Pos2::new(inner.left() + span.start_col as f32 * m.cell_w, inner.top() + span.row as f32 * m.cell_h),
        Vec2::new((span.end_col.saturating_sub(span.start_col)) as f32 * m.cell_w, m.cell_h),
    )
}

fn paint_link_spans(painter: &egui::Painter, inner: Rect, m: Metrics, spans: &[RowSpan], accent: Color32) {
    for &s in spans {
        let r = span_rect(inner, m, s);
        painter.rect_filled(r, CornerRadius::same(2), accent.gamma_multiply(0.10));
        let y = (r.bottom() - 1.5).round();
        painter.line_segment([Pos2::new(r.left(), y), Pos2::new(r.right(), y)], Stroke::new(1.5, accent));
    }
}

/// Opens Explorer with `path` selected (or opens the folder if `path` is a directory).
pub fn reveal_in_explorer(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        let _ = crate::remote_edit::open_with_system(path);
        return Ok(());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let win_path = path.display().to_string().replace('/', "\\");
        std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{win_path}\"")).spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg("-R").arg(path).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = path.parent().unwrap_or(path);
        std::process::Command::new("xdg-open").arg(dir).spawn()?;
    }
    Ok(())
}

pub fn open_url(url: &str) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer.exe").raw_arg(format!("\"{url}\"")).spawn()?;
    }
    #[cfg(target_os = "macos")]
    std::process::Command::new("open").arg(url).spawn()?;
    #[cfg(all(unix, not(target_os = "macos")))]
    std::process::Command::new("xdg-open").arg(url).spawn()?;
    Ok(())
}

// ---- Logical line & link extraction ------------------------------------------------

struct LogicalCell {
    row: u16,
    col: u16,
    width: u16,
    byte_start: usize,
    byte_end: usize,
}

struct LogicalLine {
    text: String,
    cells: Vec<LogicalCell>,
}

impl LogicalLine {
    /// Extracts the wrapped logical line covering `target_row` from `screen`.
    fn extract(screen: &vt100::Screen, target_row: u16) -> Option<(Self, usize)> {
        let (rows, cols) = screen.size();
        if target_row >= rows || cols == 0 {
            return None;
        }
        let mut start_row = target_row;
        while start_row > 0 && screen.row_wrapped(start_row - 1) {
            start_row -= 1;
        }
        let mut end_row = target_row;
        while end_row + 1 < rows && screen.row_wrapped(end_row) {
            end_row += 1;
        }
        let mut text = String::new();
        let mut cells = Vec::new();
        for r in start_row..=end_row {
            for c in 0..cols {
                let Some(cell) = screen.cell(r, c) else { break };
                if cell.is_wide_continuation() {
                    continue;
                }
                let width = if cell.is_wide() { 2 } else { 1 };
                let s = cell.contents();
                let ch = if s.is_empty() { " " } else { s };
                let byte_start = text.len();
                text.push_str(ch);
                let byte_end = text.len();
                cells.push(LogicalCell { row: r, col: c, width, byte_start, byte_end });
            }
            if r == end_row {
                // Trim trailing spaces on the final row of the logical line.
                while cells.last().is_some_and(|cell| &text[cell.byte_start..cell.byte_end] == " ") {
                    let last = cells.pop().unwrap();
                    text.truncate(last.byte_start);
                }
            }
        }
        let cursor_cell = cells.iter().position(|cell| cell.row == target_row)?;
        Some((Self { text, cells }, cursor_cell))
    }

    fn byte_index_at(&self, row: u16, col: u16) -> Option<usize> {
        self.cells.iter().find(|c| c.row == row && col >= c.col && col < c.col + c.width).map(|c| c.byte_start)
    }

    fn spans_for_bytes(&self, start_byte: usize, end_byte: usize) -> Vec<RowSpan> {
        let mut spans: Vec<RowSpan> = Vec::new();
        for c in &self.cells {
            if c.byte_end <= start_byte || c.byte_start >= end_byte {
                continue;
            }
            match spans.last_mut() {
                Some(last) if last.row == c.row && last.end_col == c.col => {
                    last.end_col = c.col + c.width;
                }
                _ => spans.push(RowSpan { row: c.row, start_col: c.col, end_col: c.col + c.width }),
            }
        }
        spans
    }
}

fn detect_at(screen: &vt100::Screen, row: u16, col: u16, cwd: Option<&str>, is_local: bool, cache: &mut PathProbeCache) -> Option<DetectedLink> {
    let (line, _) = LogicalLine::extract(screen, row)?;
    let byte_idx = line.byte_index_at(row, col)?;
    let (raw, target, (start_b, end_b)) = find_link_in_str(&line.text, byte_idx, cwd, is_local, cache)?;
    let spans = line.spans_for_bytes(start_b, end_b);
    (!spans.is_empty()).then_some(DetectedLink { raw, target, spans })
}

/// Finds a link covering `byte_idx` in `text`. Returns `(raw_path, LinkTarget, (start_byte, end_byte))`.
pub fn find_link_in_str(
    text: &str,
    byte_idx: usize,
    cwd: Option<&str>,
    is_local: bool,
    cache: &mut PathProbeCache,
) -> Option<(String, LinkTarget, (usize, usize))> {
    if byte_idx >= text.len() || !text.is_char_boundary(byte_idx) {
        return None;
    }
    let ch_at = text[byte_idx..].chars().next()?;
    if ch_at.is_whitespace() {
        return None;
    }
    // 1. Check if cursor is inside a quoted span ("...", '...', `...`) that resolves as a path/URL.
    if let Some((q_start, q_end)) = enclosing_quotes(text, byte_idx)
        && let Some(hit) = classify_candidate(&text[q_start..q_end], q_start, cwd, is_local, cache)
    {
        return Some(hit);
    }
    // 2. Otherwise inspect the whitespace-delimited token around `byte_idx`.
    let start = text[..byte_idx].rfind(char::is_whitespace).map_or(0, |i| i + text[i..].chars().next().map_or(1, char::len_utf8));
    let end = text[byte_idx..].find(char::is_whitespace).map_or(text.len(), |i| byte_idx + i);
    let token = &text[start..end];
    if !is_local && let Some((path_start, path_end)) = prompt_path(token) {
        let path_start = start + path_start;
        let path_end = start + path_end;
        if (path_start..path_end).contains(&byte_idx) {
            let (raw, mut target, span) = classify_candidate(&text[path_start..path_end], path_start, cwd, false, cache)?;
            // A shell can switch users with su/sudo while SFTP keeps the original
            // authenticated account. Preserve whose home the prompt's ~ denotes.
            if let LinkTarget::RemotePath { path, .. } = &mut target
                && (path == "~" || path.starts_with("~/"))
            {
                let at = token.find('@')?;
                let user = token[..at].trim_start_matches(['[', '(', '{']);
                if crate::ssh::cwd::valid_home_user(user) {
                    path.insert_str(1, user);
                }
            }
            return Some((raw, target, span));
        }
        return None;
    }
    classify_candidate(token, start, cwd, is_local, cache)
}

/// The directory in a conventional SSH prompt: `[user@host:~/project]$`.
/// Only the path is linked; the host and prompt marker remain ordinary text.
fn prompt_path(token: &str) -> Option<(usize, usize)> {
    let at = token.find('@')?;
    let colon = token[at + 1..].find(":/").or_else(|| token[at + 1..].find(":~"))? + at + 1;
    let host = &token[at + 1..colon];
    if host.is_empty() || !host.chars().all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_')) {
        return None;
    }
    let start = colon + 1;
    let end = token.trim_end_matches(['$', '#', '%', '❯', ']', ')', '}']).len();
    (end > start).then_some((start, end))
}

fn enclosing_quotes(text: &str, byte_idx: usize) -> Option<(usize, usize)> {
    if !text.is_char_boundary(byte_idx) {
        return None;
    }
    for quote in ['"', '\'', '`'] {
        let before = text[..byte_idx].rfind(quote);
        if let Some(open) = before {
            let inner_start = open + 1;
            if inner_start <= byte_idx
                && let Some(rel_close) = text[byte_idx..].find(quote)
            {
                let close = byte_idx + rel_close;
                if close > inner_start && close - inner_start <= 512 {
                    return Some((inner_start, close));
                }
            }
        }
    }
    None
}

fn classify_candidate(
    raw_token: &str,
    base_offset: usize,
    cwd: Option<&str>,
    is_local: bool,
    cache: &mut PathProbeCache,
) -> Option<(String, LinkTarget, (usize, usize))> {
    let (trimmed, lead, _) = trim_wrappers(raw_token);
    if trimmed.len() < 2 && (is_local || !matches!(trimmed, "/" | "~")) {
        return None;
    }
    let span_start = base_offset + lead;
    let span_end = span_start + trimmed.len();

    // URLs (`https://...`, `http://...`, `file://...`)
    if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        return Some((trimmed.to_string(), LinkTarget::Url(trimmed.to_string()), (span_start, span_end)));
    }
    if let Some(local) = crate::ssh::terminal::osc7_path(trimmed.as_bytes()).filter(|_| trimmed.starts_with("file://")) {
        if is_local {
            let p = PathBuf::from(&local);
            let is_dir = p.is_dir();
            return Some((local, LinkTarget::LocalPath { resolved: p, is_dir, line: None, col: None }, (span_start, span_end)));
        }
        return Some((local.clone(), LinkTarget::RemotePath { path: local, line: None }, (span_start, span_end)));
    }

    // Strip optional `:line[:col]` or `(line[,col])` suffix.
    let (path_part, line, col) = split_line_col(trimmed);
    if !looks_like_path_candidate(path_part) && (is_local || !matches!(path_part, "/" | "~")) {
        return None;
    }

    if is_local {
        if let Some((resolved, is_dir)) = cache.resolve(cwd, path_part) {
            return Some((path_part.to_string(), LinkTarget::LocalPath { resolved, is_dir, line, col }, (span_start, span_end)));
        }
    } else if is_remote_path_syntax(path_part) {
        let resolved = match cwd {
            Some(c) if !path_part.starts_with('/') && !path_part.starts_with('~') => {
                crate::ui::path_complete::normalize_remote(&crate::ssh::sftp::join(c, path_part))
            }
            _ => path_part.to_string(),
        };
        return Some((path_part.to_string(), LinkTarget::RemotePath { path: resolved, line }, (span_start, span_end)));
    }
    None
}

/// Trims surrounding punctuation/quotes/brackets from a token while returning byte offsets.
fn trim_wrappers(s: &str) -> (&str, usize, usize) {
    let mut start = 0;
    let mut end = s.len();
    const LEAD: &[char] = &['"', '\'', '`', '(', '[', '<', '{'];
    const TRAIL: &[char] = &['"', '\'', '`', ')', ']', '>', '}', ',', ';', '?'];
    while start < end {
        let ch = s[start..].chars().next().unwrap();
        if LEAD.contains(&ch) {
            start += ch.len_utf8();
        } else {
            break;
        }
    }
    while end > start {
        let ch = s[..end].chars().next_back().unwrap();
        if TRAIL.contains(&ch) || (ch == '.' && !s[start..end].ends_with("..")) || (ch == ':' && !is_drive_root(&s[start..end])) {
            end -= ch.len_utf8();
        } else {
            break;
        }
    }
    (&s[start..end], start, s.len() - end)
}

fn is_drive_root(s: &str) -> bool {
    s.len() == 2 && s.as_bytes()[0].is_ascii_alphabetic() && s.as_bytes()[1] == b':'
}

/// Splits `path:42:10` or `path(42,10)` into `(path, Some(42), Some(10))`, keeping Windows
/// drive letters (`C:\foo.rs:42`).
pub fn split_line_col(s: &str) -> (&str, Option<u32>, Option<u32>) {
    // `file.rs(12,4)` or `file.rs(12)`
    if let Some(open) = s.rfind('(')
        && s.ends_with(')')
        && open > 0
    {
        let inside = &s[open + 1..s.len() - 1];
        let mut parts = inside.split(',');
        if let Some(l) = parts.next().and_then(|p| p.trim().parse::<u32>().ok()) {
            let c = parts.next().and_then(|p| p.trim().parse::<u32>().ok());
            if parts.next().is_none() {
                return (&s[..open], Some(l), c);
            }
        }
    }
    // `file.rs:12:4` or `file.rs:12`
    let mut rest = s;
    let mut nums = Vec::new();
    while let Some(colon) = rest.rfind(':') {
        // Don't split a Windows drive prefix `C:`.
        if colon <= 1 && rest.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) {
            break;
        }
        let tail = &rest[colon + 1..];
        if !tail.is_empty()
            && tail.bytes().all(|b| b.is_ascii_digit())
            && let Ok(n) = tail.parse::<u32>()
        {
            nums.push(n);
            rest = &rest[..colon];
            if nums.len() == 2 {
                break;
            }
        } else {
            break;
        }
    }
    match nums.as_slice() {
        [col, line] => (rest, Some(*line), Some(*col)),
        [line] => (rest, Some(*line), None),
        _ => (s, None, None),
    }
}

fn looks_like_path_candidate(s: &str) -> bool {
    if s.is_empty() || s == "." || s == ".." || s == "/" || s == "\\" || s == "~" {
        return false;
    }
    if s.contains("://") {
        return false;
    }
    // Skip plain numbers, flags (`--foo`, `-v`), and pure punctuation.
    if s.starts_with('-') || s.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return false;
    }
    s.chars().any(|c| c.is_alphanumeric())
}

fn is_remote_path_syntax(s: &str) -> bool {
    if s.starts_with('~') || s.starts_with('/') || s.starts_with("./") || s.starts_with("../") {
        return true;
    }
    // Remote folders do not need an extension or a trailing slash.
    s.contains('/') && !s.contains(':')
}

/// Resolves `raw` against `cwd` (or home / root / MSYS / WSL drive mounts) and returns
/// `Some((canonical_or_joined_path, is_dir))` when it exists on disk.
pub fn resolve_local_path(cwd: Option<&str>, raw: &str) -> Option<(PathBuf, bool)> {
    let raw = raw.trim();
    if !looks_like_path_candidate(raw) {
        return None;
    }
    // Strip git diff `a/` or `b/` prefix when the rest exists in `cwd`.
    let candidates: &[&str] = if let Some(stripped) = raw.strip_prefix("a/").or_else(|| raw.strip_prefix("b/")) { &[raw, stripped] } else { &[raw] };
    for &item in candidates {
        if let Some(hit) = try_resolve_one(cwd, item) {
            return Some(hit);
        }
    }
    None
}

fn try_resolve_one(cwd: Option<&str>, raw: &str) -> Option<(PathBuf, bool)> {
    let expanded = expand_tilde_and_msys(raw);
    let path = Path::new(&expanded);
    if path.is_absolute() {
        let meta = std::fs::metadata(path).ok()?;
        return Some((clean_path(path), meta.is_dir()));
    }
    let base = cwd.filter(|c| !c.is_empty()).map(PathBuf::from).or_else(|| std::env::current_dir().ok())?;
    let joined = base.join(path);
    let meta = std::fs::metadata(&joined).ok()?;
    Some((clean_path(&joined), meta.is_dir()))
}

fn expand_tilde_and_msys(raw: &str) -> String {
    if raw == "~" || raw.starts_with("~/") || raw.starts_with(r"~\") {
        if let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) {
            let rest = raw.strip_prefix('~').unwrap_or_default().trim_start_matches(['/', '\\']);
            return home.join(rest).display().to_string();
        }
    }
    #[cfg(windows)]
    {
        // Git Bash `/d/Projects/...` or WSL `/mnt/d/Projects/...` on Windows.
        let msys = raw.strip_prefix("/mnt/").or_else(|| raw.strip_prefix('/'));
        if let Some(rest) = msys
            && rest.len() >= 2
            && rest.as_bytes()[0].is_ascii_alphabetic()
            && rest.as_bytes()[1] == b'/'
        {
            let drive = rest.as_bytes()[0] as char;
            let tail = &rest[2..];
            let win = format!("{drive}:\\{}", tail.replace('/', "\\"));
            if Path::new(&win).exists() {
                return win;
            }
        }
    }
    raw.to_string()
}

fn clean_path(path: &Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(canon) => {
            let s = canon.display().to_string();
            // Strip Windows `\\?\` extended-length prefix for clean display & clipboard.
            if let Some(stripped) = s.strip_prefix(r"\\?\") { PathBuf::from(stripped) } else { canon }
        }
        Err(_) => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_prompt_links_only_the_directory_and_opens_sftp_on_click() {
        let mut cache = PathProbeCache::default();
        for (prompt, expected, resolved) in [
            (
                "prisma@na-prisma-02:~/servers/practice-servers/na-practice-01$",
                "~/servers/practice-servers/na-practice-01",
                "~prisma/servers/practice-servers/na-practice-01",
            ),
            ("root@server:/srv/app#", "/srv/app", "/srv/app"),
            ("[dev@host:~/repo]$", "~/repo", "~dev/repo"),
            ("dev@host:~$", "~", "~dev"),
            ("root@host:/#", "/", "/"),
        ] {
            let start = prompt.find(':').unwrap() + 1;
            let (raw, target, span) = find_link_in_str(prompt, start, Some("/old/cwd"), false, &mut cache).unwrap();
            assert_eq!(raw, expected);
            assert_eq!(&prompt[span.0..span.1], expected);
            assert_eq!(target, LinkTarget::RemotePath { path: resolved.into(), line: None });
            assert!(find_link_in_str(prompt, 2, None, false, &mut cache).is_none());
            let link = DetectedLink { raw, target, spans: Vec::new() };
            let mut links = LinksState { hovered: Some(link), ..Default::default() };
            assert_eq!(links.on_click(Pos2::ZERO, false, &egui::Context::default()), (true, Some(LinkAction::OpenSftp(resolved.into()))));
            assert!(!links.has_open_menu());
        }
        for folder in ["practice-01/", "servers/practice-01", "~/servers", "/", "~", "~prisma/servers"] {
            assert!(find_link_in_str(folder, 0, None, false, &mut cache).is_some(), "{folder}");
        }
    }

    #[test]
    fn remote_path_menu_returns_an_sftp_action() {
        use egui_kittest::kittest::Queryable;
        let link = DetectedLink {
            raw: "/srv/project".into(),
            target: LinkTarget::RemotePath { path: "/srv/project".into(), line: None },
            spans: vec![RowSpan { row: 0, start_col: 0, end_col: 12 }],
        };
        let mut links = LinksState { hovered: Some(link), ..Default::default() };
        assert!(links.on_secondary_click(egui::pos2(30.0, 12.0)));
        let result = std::sync::Arc::new(std::sync::Mutex::new(None));
        let output = result.clone();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(800.0, 600.0)).build_ui(move |ui| {
            let painter = ui.painter().clone();
            let m = Metrics { cell_w: 8.0, cell_h: 18.0, text_dy: 0.0 };
            if let Some(action) = links.paint_and_menu(ui, &painter, ui.max_rect(), m, theme::accent()) {
                *output.lock().unwrap() = Some(action);
            }
        });
        h.run_steps(5);
        h.get_by_label("📁  Open in SFTP").click();
        h.run_steps(3);
        assert_eq!(result.lock().unwrap().take(), Some(LinkAction::OpenSftp("/srv/project".into())));
    }

    #[test]
    fn splits_line_and_column_suffixes() {
        assert_eq!(split_line_col("src/main.rs:42:10"), ("src/main.rs", Some(42), Some(10)));
        assert_eq!(split_line_col("src/main.rs:42"), ("src/main.rs", Some(42), None));
        assert_eq!(split_line_col(r"C:\proj\src\lib.rs:100:5"), (r"C:\proj\src\lib.rs", Some(100), Some(5)));
        assert_eq!(split_line_col(r"C:\proj\src\lib.rs"), (r"C:\proj\src\lib.rs", None, None));
        assert_eq!(split_line_col("src/app.rs(88,12)"), ("src/app.rs", Some(88), Some(12)));
    }

    #[test]
    fn detects_existing_relative_and_absolute_files() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("src");
        std::fs::create_dir_all(&sub).unwrap();
        let file = sub.join("hello.rs");
        std::fs::write(&file, "fn main() {}").unwrap();

        let cwd = dir.path().to_str().unwrap();
        let mut cache = PathProbeCache::default();

        // Relative path with line:col inside prose
        let line = "Claude edited `src/hello.rs:12:4` successfully.";
        let idx = line.find("hello.rs").unwrap();
        let (raw, target, _) = find_link_in_str(line, idx, Some(cwd), true, &mut cache).expect("should detect relative path");
        assert_eq!(raw, "src/hello.rs");
        match target {
            LinkTarget::LocalPath { resolved, is_dir, line, col } => {
                assert!(!is_dir);
                assert!(resolved.ends_with("hello.rs"));
                assert_eq!((line, col), (Some(12), Some(4)));
            }
            _ => panic!("expected LocalPath"),
        }

        // Non-existent words are ignored
        let neg = "running 23 tests in N/A mode";
        assert!(find_link_in_str(neg, 2, Some(cwd), true, &mut cache).is_none());
        assert!(find_link_in_str(neg, neg.find("N/A").unwrap(), Some(cwd), true, &mut cache).is_none());
    }

    #[test]
    fn detects_urls_and_remote_paths() {
        let mut cache = PathProbeCache::default();
        let text = "Visit https://github.com/izLoki/Tether, or check /etc/nginx/nginx.conf:10.";
        let u_idx = text.find("github").unwrap();
        let (_, target, _) = find_link_in_str(text, u_idx, None, true, &mut cache).unwrap();
        assert_eq!(target, LinkTarget::Url("https://github.com/izLoki/Tether".into()));

        let p_idx = text.find("nginx.conf").unwrap();
        let (raw, r_target, _) = find_link_in_str(text, p_idx, Some("/var"), false, &mut cache).unwrap();
        assert_eq!(raw, "/etc/nginx/nginx.conf");
        assert_eq!(r_target, LinkTarget::RemotePath { path: "/etc/nginx/nginx.conf".into(), line: Some(10) });
    }

    #[test]
    fn multibyte_utf8_claude_lines_never_panic() {
        let mut cache = PathProbeCache::default();
        let line = "╭─── ✳ Claude Code ● ⎿ Read(\"src/main.rs\") ─── 📁 🚀 áéíóú ───╮";
        for idx in 0..=line.len() + 2 {
            let _ = find_link_in_str(line, idx, None, false, &mut cache);
        }
        let target_idx = line.find("main.rs").unwrap();
        let (raw, _, _) = find_link_in_str(line, target_idx, Some("/app"), false, &mut cache).unwrap();
        assert_eq!(raw, "src/main.rs");
    }
}
