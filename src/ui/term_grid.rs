//! Cached painting of the vt100 cell grid.
//!
//! Each visible row is reduced to background spans plus one laid-out galley. Rows are
//! cached by a hash of their cells, so an idle terminal reuses everything without touching
//! the parser, and a streaming one only lays out rows whose content actually changed
//! (scrolled rows are found by hash at their new position).
//!
//! The work is split so the terminal mutex is held only for cheap reads:
//! [`GridCache::sync`] runs under the lock (hash rows, build layout jobs for new ones),
//! [`GridCache::layout`] lays them out after the lock is released, and
//! [`GridCache::paint`] only emits shapes.

use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui::{self, Color32, FontId, Galley, Pos2, Rect, Stroke, Vec2, text::LayoutJob};

use super::terminal_view::Metrics;
use crate::theme::{self, Palette};

/// What laid-out galleys depend on in the context: the loaded fonts and the scale they
/// were rasterised at (UI scale setting, moving to a monitor with another DPI).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderKey {
    pub fonts: u64,
    pub pixels_per_point: f32,
}

impl RenderKey {
    pub fn of(ctx: &egui::Context) -> Self {
        Self { fonts: crate::fonts::version(ctx), pixels_per_point: ctx.pixels_per_point() }
    }
}

/// Everything a cached row's pixels depend on besides its cells.
#[derive(Debug, Clone, Copy, PartialEq)]
struct StyleKey {
    palette: u64,
    font_size: f32,
    render: RenderKey,
}

/// Inputs of a whole frame; when unchanged the previous frame's rows are reused as is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameKey {
    pub generation: u64,
    pub scroll: usize,
    pub size: (u16, u16),
    palette: u64,
    font_size: f32,
    render: RenderKey,
}

impl FrameKey {
    pub fn new(generation: u64, scroll: usize, size: (u16, u16), font: &FontId, render: RenderKey) -> Self {
        Self { generation, scroll, size, palette: theme::palette_version(), font_size: font.size, render }
    }

    fn style(&self) -> StyleKey {
        StyleKey { palette: self.palette, font_size: self.font_size, render: self.render }
    }
}

/// A run of cells sharing one background colour, in cell units.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BgSpan {
    col: u16,
    len: u16,
    color: Color32,
}

#[derive(Default)]
struct RowPaint {
    galley: Option<Arc<Galley>>,
    bgs: Vec<BgSpan>,
    /// Start columns of double-width cells (selection edges must cover both halves).
    wide: Vec<u16>,
}

/// A row seen for the first time this frame, waiting for layout outside the lock.
struct Pending {
    row: usize,
    hash: u64,
    job: Option<LayoutJob>,
    bgs: Vec<BgSpan>,
    wide: Vec<u16>,
}

#[derive(Default)]
pub struct GridCache {
    key: Option<FrameKey>,
    style: Option<StyleKey>,
    /// Per visible row: content hash and its paint data.
    rows: Vec<(u64, Arc<RowPaint>)>,
    pending: Vec<Pending>,
}

impl GridCache {
    /// Under the terminal lock: brings the row list up to date with `screen`.
    /// Cheap no-op when `key` matches the previous frame.
    pub fn sync(&mut self, key: FrameKey, screen: &vt100::Screen, palette: &Palette, font: &FontId) {
        if self.key == Some(key) {
            return;
        }
        if self.style != Some(key.style()) {
            self.rows.clear();
            self.style = Some(key.style());
        }
        self.key = Some(key);
        let (rows, cols) = key.size;
        let known: HashMap<u64, Arc<RowPaint>> = self.rows.drain(..).collect();
        self.pending.clear();
        for row in 0..rows {
            let hash = row_hash(screen, row, cols);
            match known.get(&hash) {
                Some(paint) => self.rows.push((hash, paint.clone())),
                None => {
                    let (job, bgs, wide) = build_row(screen, row, cols, palette, font);
                    self.pending.push(Pending { row: row as usize, hash, job, bgs, wide });
                    self.rows.push((hash, Arc::new(RowPaint::default())));
                }
            }
        }
    }

    /// After the lock is released: lays out the rows `sync` found new.
    pub fn layout(&mut self, ctx: &egui::Context) {
        if self.pending.is_empty() {
            return;
        }
        let mut laid: HashMap<u64, Arc<RowPaint>> = HashMap::with_capacity(self.pending.len());
        for p in self.pending.drain(..) {
            let paint = laid
                .entry(p.hash)
                .or_insert_with(|| {
                    let galley = p.job.map(|job| ctx.fonts_mut(|f| f.layout_job(job)));
                    Arc::new(RowPaint { galley, bgs: p.bgs, wide: p.wide })
                })
                .clone();
            self.rows[p.row] = (p.hash, paint);
        }
    }

    /// Emits the grid's shapes. `selection(row)` gives the selected `[start, end)` columns.
    pub fn paint(&self, painter: &egui::Painter, origin: Pos2, m: Metrics, selection: impl Fn(u16) -> Option<(u16, u16)>) {
        let Metrics { cell_w, cell_h, text_dy } = m;
        let cols = self.key.map_or(0, |k| k.size.1);
        let span_rect = |y: f32, col: u16, len: u16| Rect::from_min_size(Pos2::new(origin.x + col as f32 * cell_w, y), Vec2::new(len as f32 * cell_w, cell_h));
        let sel_color = theme::term_selection();
        let term_fg = theme::term_fg();
        for (row, (_, paint)) in self.rows.iter().enumerate() {
            let y = origin.y + row as f32 * cell_h;
            for bg in &paint.bgs {
                painter.rect_filled(span_rect(y, bg.col, bg.len), 0.0, bg.color);
            }
            if let Some((start, end)) = selection(row as u16).and_then(|(s, e)| selected_span(&paint.wide, s, e, cols)) {
                painter.rect_filled(span_rect(y, start, end - start), 0.0, sel_color);
            }
            if let Some(galley) = &paint.galley {
                painter.galley(Pos2::new(origin.x, y + text_dy), galley.clone(), term_fg);
            }
        }
    }
}

/// Snaps a selected column range to whole cells: a wide cell counts when its first half is
/// selected and is then highlighted across both halves. `None` when nothing is selected.
fn selected_span(wide: &[u16], start: u16, end: u16, cols: u16) -> Option<(u16, u16)> {
    let is_wide = |c: u16| wide.binary_search(&c).is_ok();
    let start = if start > 0 && is_wide(start - 1) { start + 1 } else { start };
    let end = end.min(cols);
    let end = if end > 0 && is_wide(end - 1) { end + 1 } else { end };
    (start < end).then_some((start, end))
}

#[derive(Clone, Copy, PartialEq)]
pub struct Style {
    pub fg: Color32,
    pub bg: Option<Color32>,
    pub italic: bool,
    pub underline: bool,
}

pub fn cell_style(cell: &vt100::Cell, p: &Palette) -> Style {
    let mut fg = match cell.fgcolor() {
        vt100::Color::Default => p.term_fg,
        vt100::Color::Idx(i) if cell.bold() && i < 8 => theme::indexed_in(p, i + 8),
        vt100::Color::Idx(i) => theme::indexed_in(p, i),
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    };
    let mut bg = match cell.bgcolor() {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(theme::indexed_in(p, i)),
        vt100::Color::Rgb(r, g, b) => Some(Color32::from_rgb(r, g, b)),
    };
    if cell.inverse() {
        let new_bg = fg;
        fg = bg.unwrap_or(p.term_bg);
        bg = Some(new_bg);
    }
    if cell.dim() {
        fg = fg.gamma_multiply(0.6);
    }
    Style { fg, bg, italic: cell.italic(), underline: cell.underline() }
}

fn color_bits(c: vt100::Color) -> u64 {
    match c {
        vt100::Color::Default => 0,
        vt100::Color::Idx(i) => 0x100 | u64::from(i),
        vt100::Color::Rgb(r, g, b) => 0x100_0000 | (u64::from(r) << 16) | (u64::from(g) << 8) | u64::from(b),
    }
}

/// Hash of everything that affects how a row looks (contents, colours, attributes, widths).
fn row_hash(screen: &vt100::Screen, row: u16, cols: u16) -> u64 {
    let mut h = RowHasher::default();
    h.word(u64::from(cols));
    for col in 0..cols {
        let Some(cell) = screen.cell(row, col) else { break };
        let flags = u64::from(cell.bold())
            | u64::from(cell.dim()) << 1
            | u64::from(cell.italic()) << 2
            | u64::from(cell.underline()) << 3
            | u64::from(cell.inverse()) << 4
            | u64::from(cell.is_wide()) << 5
            | u64::from(cell.is_wide_continuation()) << 6;
        let text = cell.contents().as_bytes();
        h.word(color_bits(cell.fgcolor()) | color_bits(cell.bgcolor()) << 25 | flags << 50 | (text.len() as u64) << 57);
        for chunk in text.chunks(8) {
            let mut buf = [0u8; 8];
            buf[..chunk.len()].copy_from_slice(chunk);
            h.word(u64::from_le_bytes(buf));
        }
    }
    h.finish()
}

/// Fast word-at-a-time hash for [`row_hash`] (it runs over every visible cell whenever a
/// terminal changes, so SipHash was measurable). Multiply-rotate per word plus a murmur3
/// finaliser; a collision would only show a stale row until that row changes again.
#[derive(Default)]
struct RowHasher(u64);

impl RowHasher {
    const K: u64 = 0x9E37_79B9_7F4A_7C15;

    #[inline]
    fn word(&mut self, w: u64) {
        self.0 = (self.0 ^ w).wrapping_mul(Self::K).rotate_left(29);
    }

    fn finish(&self) -> u64 {
        let mut x = self.0;
        x ^= x >> 33;
        x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        x ^= x >> 33;
        x = x.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
        x ^ (x >> 33)
    }
}

/// Background spans, double-width columns and the text layout job for one row.
fn build_row(screen: &vt100::Screen, row: u16, cols: u16, palette: &Palette, font: &FontId) -> (Option<LayoutJob>, Vec<BgSpan>, Vec<u16>) {
    let mut job = LayoutJob::default();
    let mut bgs: Vec<BgSpan> = Vec::new();
    let mut wide = Vec::new();
    let mut run = String::new();
    let mut run_style: Option<Style> = None;
    let flush = |job: &mut LayoutJob, run: &mut String, style: Option<Style>| {
        if let Some(s) = style.filter(|_| !run.is_empty()) {
            let mut fmt = egui::TextFormat::simple(font.clone(), s.fg);
            fmt.italics = s.italic;
            if s.underline {
                fmt.underline = Stroke::new(1.0, s.fg);
            }
            job.append(run, 0.0, fmt);
            run.clear();
        }
    };
    for col in 0..cols {
        let Some(cell) = screen.cell(row, col) else { break };
        if cell.is_wide_continuation() {
            continue;
        }
        let style = cell_style(cell, palette);
        let width = if cell.is_wide() { 2 } else { 1 };
        if cell.is_wide() {
            wide.push(col);
        }
        if let Some(color) = style.bg {
            match bgs.last_mut() {
                Some(last) if last.color == color && last.col + last.len == col => last.len += width,
                _ => bgs.push(BgSpan { col, len: width, color }),
            }
        }
        if run_style != Some(style) {
            flush(&mut job, &mut run, run_style);
            run_style = Some(style);
        }
        let text = cell.contents();
        run.push_str(if text.is_empty() { " " } else { text });
    }
    flush(&mut job, &mut run, run_style);
    if job.sections.is_empty() {
        return (None, bgs, wide);
    }
    job.wrap.max_width = f32::INFINITY;
    (Some(job), bgs, wide)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser(input: &str) -> vt100::Parser {
        let mut p = vt100::Parser::new(4, 20, 100);
        p.process(input.as_bytes());
        p
    }

    #[test]
    fn inverse_swaps_colors() {
        let p = parser("\x1b[7mX");
        let pal = Palette::TETHER_NIGHT;
        let s = cell_style(p.screen().cell(0, 0).unwrap(), &pal);
        assert_eq!(s.fg, pal.term_bg);
        assert_eq!(s.bg, Some(pal.term_fg));
    }

    #[test]
    fn bold_brightens_low_ansi() {
        let p = parser("\x1b[1;31mX");
        let pal = Palette::TETHER_NIGHT;
        assert_eq!(cell_style(p.screen().cell(0, 0).unwrap(), &pal).fg, pal.ansi[9]);
    }

    #[test]
    fn backgrounds_merge_into_spans() {
        let p = parser("\x1b[41mabc\x1b[42mde\x1b[0m f\x1b[41m漢g");
        let (_, bgs, wide) = build_row(p.screen(), 0, 20, &Palette::TETHER_NIGHT, &FontId::monospace(12.0));
        let red = Palette::TETHER_NIGHT.ansi[1];
        let green = Palette::TETHER_NIGHT.ansi[2];
        assert_eq!(bgs, vec![BgSpan { col: 0, len: 3, color: red }, BgSpan { col: 3, len: 2, color: green }, BgSpan { col: 7, len: 3, color: red }]);
        assert_eq!(wide, vec![7]);
    }

    #[test]
    fn row_hash_sees_content_and_style() {
        let a = parser("hello");
        let b = parser("hellp");
        let c = parser("\x1b[31mhello");
        let d = parser("hello");
        let h = |p: &vt100::Parser| row_hash(p.screen(), 0, 20);
        assert_ne!(h(&a), h(&b));
        assert_ne!(h(&a), h(&c));
        assert_eq!(h(&a), h(&d));
        assert_eq!(row_hash(a.screen(), 1, 20), row_hash(a.screen(), 2, 20), "blank rows share a hash");
    }

    #[test]
    fn selection_snaps_to_wide_cells() {
        // A wide cell at 4 (continuation at 5).
        assert_eq!(selected_span(&[4], 2, 5, 20), Some((2, 6)));
        assert_eq!(selected_span(&[4], 5, 9, 20), Some((6, 9)));
        assert_eq!(selected_span(&[], 3, 30, 20), Some((3, 20)));
        assert_eq!(selected_span(&[4], 5, 6, 20), None);
    }

    #[test]
    fn sync_reuses_rows_and_lays_out_only_new_ones() {
        let mut p = vt100::Parser::new(3, 10, 100);
        p.process(b"one\r\ntwo\r\nthree");
        let ctx = egui::Context::default();
        let font = FontId::monospace(12.0);
        let pal = Palette::TETHER_NIGHT;
        let mut cache = GridCache::default();
        let r = RenderKey { fonts: 0, pixels_per_point: 1.0 };
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            cache.sync(FrameKey::new(1, 0, (3, 10), &font, r), p.screen(), &pal, &font);
            assert_eq!(cache.pending.len(), 3);
            cache.layout(ui.ctx());
            assert!(cache.rows.iter().all(|(_, r)| r.galley.is_some()));
            let first = cache.rows[1].1.clone();
            // Same key: nothing to do.
            cache.sync(FrameKey::new(1, 0, (3, 10), &font, r), p.screen(), &pal, &font);
            assert!(cache.pending.is_empty());
            // Scroll one line: "two" moves up and is reused, only the new last row is built.
            p.process(b"\r\nfour");
            cache.sync(FrameKey::new(2, 0, (3, 10), &font, r), p.screen(), &pal, &font);
            assert_eq!(cache.pending.len(), 1);
            cache.layout(ui.ctx());
            assert!(Arc::ptr_eq(&cache.rows[0].1, &first));
            // A palette change rebuilds everything.
            let mut key = FrameKey::new(2, 0, (3, 10), &font, r);
            key.palette += 1;
            cache.sync(key, p.screen(), &pal, &font);
            assert_eq!(cache.pending.len(), 3);
            cache.layout(ui.ctx());
            // So do new fonts and a new scale: old galleys would be garbled or blurry.
            let fonts = RenderKey { fonts: 1, ..r };
            cache.sync(FrameKey::new(2, 0, (3, 10), &font, fonts), p.screen(), &pal, &font);
            assert_eq!(cache.pending.len(), 3);
            cache.layout(ui.ctx());
            let scaled = RenderKey { pixels_per_point: 1.5, ..fonts };
            cache.sync(FrameKey::new(2, 0, (3, 10), &font, scaled), p.screen(), &pal, &font);
            assert_eq!(cache.pending.len(), 3);
        });
        out.textures_delta.clear();
    }
}
