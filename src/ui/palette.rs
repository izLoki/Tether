//! The command palette: a centred overlay with a search field over every registered
//! [`Entry`], fuzzy-ranked as you type. ↑/↓ (or the mouse) pick, Enter runs, Esc closes.

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke, Vec2, text::LayoutJob};

use crate::actions::{Command, Entry, Hit, Registry};
use crate::theme;

const WIDTH: f32 = 620.0;
const ROW_H: f32 = 40.0;
const VISIBLE_ROWS: usize = 9;
const OPEN_SECS: f32 = 0.14;

pub enum PaletteOutcome {
    Open,
    Close,
    Run(Command),
}

pub struct Palette {
    query: String,
    registry: Registry,
    hits: Vec<Hit>,
    selected: usize,
    opened_at: Option<f64>,
    /// Scroll the selection into view on the next frame (keyboard moves only).
    reveal: bool,
}

impl Palette {
    pub fn new(registry: Registry) -> Self {
        let hits = registry.search("");
        Self { query: String::new(), registry, hits, selected: 0, opened_at: None, reveal: false }
    }

    /// Opens with `query` typed in (e.g. "theme " to browse themes).
    pub fn with_query(mut self, query: &str) -> Self {
        self.query = query.to_string();
        self.hits = self.registry.search(query);
        self
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Titles of the current results, best first (tests, accessibility).
    pub fn result_titles(&self) -> Vec<&str> {
        self.hits.iter().map(|h| self.registry.entries()[h.index].title.as_str()).collect()
    }

    fn set_query(&mut self, q: String) {
        if q != self.query {
            self.hits = self.registry.search(&q);
            self.query = q;
            self.selected = 0;
            self.reveal = true;
        }
    }

    fn step(&mut self, delta: isize) {
        let n = self.hits.len();
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        self.reveal = true;
    }

    fn chosen(&self) -> Option<Command> {
        self.hits.get(self.selected).map(|h| self.registry.entries()[h.index].command.clone())
    }

    pub fn show(&mut self, ctx: &egui::Context) -> PaletteOutcome {
        let now = ctx.input(|i| i.time);
        let opened = *self.opened_at.get_or_insert(now);
        let dur = theme::anim_time(OPEN_SECS);
        let t = if dur <= 0.0 { 1.0 } else { ((now - opened) as f32 / dur).clamp(0.0, 1.0) };
        let eased = 1.0 - (1.0 - t).powi(3);
        if t < 1.0 {
            ctx.request_repaint();
        }
        // Keys the list handles, taken before the text field sees them.
        let (down, up, enter, page_down, page_up) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown) || i.consume_key(Modifiers::CTRL, Key::N),
                i.consume_key(Modifiers::NONE, Key::ArrowUp) || i.consume_key(Modifiers::CTRL, Key::P),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::PageDown),
                i.consume_key(Modifiers::NONE, Key::PageUp),
            )
        });
        if down {
            self.step(1);
        }
        if up {
            self.step(-1);
        }
        if page_down {
            self.step(VISIBLE_ROWS as isize);
        }
        if page_up {
            self.step(-(VISIBLE_ROWS as isize));
        }
        if enter && let Some(cmd) = self.chosen() {
            return PaletteOutcome::Run(cmd);
        }

        let screen = ctx.content_rect();
        let width = WIDTH.min(screen.width() - 32.0);
        let id = Id::new("tether_palette");
        let area = egui::Modal::default_area(id)
            .anchor(Align2::CENTER_TOP, Vec2::new(0.0, (screen.height() * 0.12).clamp(24.0, 120.0) - 8.0 * (1.0 - eased)))
            .fade_in(false);
        let dim = if theme::palette().dark { 120 } else { 60 };
        let mut run = None;
        let modal =
            egui::Modal::new(id).area(area).backdrop_color(Color32::from_black_alpha(dim).gamma_multiply(eased)).frame(egui::Frame::NONE).show(ctx, |ui| {
                ui.multiply_opacity(eased);
                run = self.window(ui, width);
            });
        if let Some(cmd) = run {
            return PaletteOutcome::Run(cmd);
        }
        if modal.should_close() { PaletteOutcome::Close } else { PaletteOutcome::Open }
    }

    fn window(&mut self, ui: &mut egui::Ui, width: f32) -> Option<Command> {
        let frame = egui::Frame::new()
            .fill(theme::surface())
            .stroke(Stroke::new(1.0, theme::border()))
            .corner_radius(CornerRadius::same(12))
            .shadow(egui::Shadow { offset: [0, 16], blur: 40, spread: 0, color: theme::shadow() })
            .inner_margin(egui::Margin::same(8));
        frame
            .show(ui, |ui| {
                ui.set_width(width);
                self.search_field(ui);
                ui.add_space(6.0);
                let run = self.results(ui);
                footer(ui);
                run
            })
            .inner
    }

    fn search_field(&mut self, ui: &mut egui::Ui) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, CornerRadius::same(8), theme::surface_2());
        super::settings_widgets::paint_icon(
            p,
            Rect::from_center_size(Pos2::new(rect.left() + 20.0, rect.center().y), Vec2::splat(15.0)),
            super::settings_widgets::Icon::Search,
            theme::text_faint(),
        );
        let edit_rect = Rect::from_min_max(Pos2::new(rect.left() + 36.0, rect.top() + 4.0), Pos2::new(rect.right() - 8.0, rect.bottom() - 4.0));
        let mut q = self.query.clone();
        let edit = ui.put(
            edit_rect,
            egui::TextEdit::singleline(&mut q)
                .frame(egui::Frame::NONE)
                .font(FontId::proportional(15.0))
                .hint_text("Run a command, open a host or a shell, switch theme…")
                .desired_width(edit_rect.width())
                .vertical_align(egui::Align::Center)
                .id(Id::new("tether_palette_query")),
        );
        if !edit.has_focus() {
            edit.request_focus();
        }
        self.set_query(q);
    }

    fn results(&mut self, ui: &mut egui::Ui) -> Option<Command> {
        if self.hits.is_empty() {
            ui.add_space(18.0);
            ui.vertical_centered(|ui| ui.label(egui::RichText::new("No matching commands").color(theme::text_faint())));
            ui.add_space(18.0);
            return None;
        }
        let mut run = None;
        let height = self.hits.len().min(VISIBLE_ROWS) as f32 * ROW_H;
        egui::ScrollArea::vertical().id_salt("palette_results").max_height(height).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (row, hit) in self.hits.iter().enumerate() {
                let entry = &self.registry.entries()[hit.index];
                let selected = row == self.selected;
                let resp = entry_row(ui, entry, hit, selected);
                if selected && std::mem::take(&mut self.reveal) {
                    resp.scroll_to_me(None);
                }
                if resp.hovered() && ui.input(|i| i.pointer.delta() != Vec2::ZERO) {
                    self.selected = row;
                }
                if resp.clicked() {
                    run = Some(entry.command.clone());
                }
            }
        });
        run
    }
}

fn entry_row(ui: &mut egui::Ui, entry: &Entry, hit: &Hit, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Palette {}", entry.title)));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let p = ui.painter();
    let card = rect.shrink2(Vec2::new(0.0, 1.0));
    if selected {
        p.rect_filled(card, CornerRadius::same(7), theme::surface_2());
        let bar = Rect::from_center_size(Pos2::new(card.left() + 1.5, card.center().y), Vec2::new(3.0, 18.0));
        p.rect_filled(bar, CornerRadius::same(2), theme::accent());
    }
    // A small icon for the kind; its name goes on the right, before the keys.
    let tile = Rect::from_center_size(Pos2::new(card.left() + 24.0, card.center().y), Vec2::splat(ICON_COL));
    p.rect_filled(tile, CornerRadius::same(6), theme::surface_3().gamma_multiply(if selected { 1.0 } else { 0.6 }));
    let icon_col = if selected { theme::accent() } else { theme::text_dim() };
    super::settings_widgets::paint_icon(p, tile.shrink(5.0), category_icon(entry.category), icon_col);
    let text_x = tile.right() + 10.0;

    // Keys on the right.
    let mut right = card.right() - 10.0;
    if let Some(keys) = &entry.keys {
        for cap in keys.split('+').collect::<Vec<_>>().into_iter().rev() {
            let g = p.layout_no_wrap(cap.to_string(), FontId::monospace(10.5), theme::text_dim());
            let r = Rect::from_min_max(Pos2::new(right - g.size().x - 10.0, card.center().y - 10.0), Pos2::new(right, card.center().y + 10.0));
            p.rect_filled(r.translate(Vec2::new(0.0, 1.0)), CornerRadius::same(4), theme::border());
            p.rect(r, CornerRadius::same(4), theme::surface_3(), Stroke::new(1.0, theme::border()), egui::StrokeKind::Inside);
            p.galley(r.center() - g.size() / 2.0, g, theme::text_dim());
            right = r.left() - 4.0;
        }
    }

    let cat = p.layout_no_wrap(entry.category.to_string(), FontId::proportional(10.5), theme::text_faint().gamma_multiply(0.8));
    right -= if entry.keys.is_some() { 8.0 } else { 0.0 };
    p.galley(Pos2::new(right - cat.size().x, card.center().y - cat.size().y / 2.0), cat.clone(), theme::text_faint());
    right -= cat.size().x;

    let clip = Rect::from_min_max(Pos2::new(text_x, card.top()), Pos2::new(right - 8.0, card.bottom()));
    let tp = p.with_clip_rect(clip);
    let galley = p.layout_job(highlighted(&entry.title, &hit.title_matches, selected));
    let title_w = galley.size().x;
    tp.galley(Pos2::new(text_x, card.center().y - galley.size().y / 2.0), galley, theme::text());
    if !entry.detail.is_empty() {
        tp.text(Pos2::new(text_x + title_w + 10.0, card.center().y), Align2::LEFT_CENTER, &entry.detail, FontId::proportional(12.0), theme::text_faint());
    }
    resp
}

/// Side of the kind icon in front of each entry.
const ICON_COL: f32 = 24.0;

fn category_icon(category: &str) -> super::settings_widgets::Icon {
    use super::settings_widgets::Icon;
    match category {
        "Shell" | "Terminal" => Icon::Terminal,
        "Host" => Icon::Server,
        "SFTP" => Icon::Folder,
        "Tab" => Icon::Rows,
        "Pane" | "Layout" | "Workspace" => Icon::Layout,
        "Agent" => Icon::Sparkle,
        "Theme" => Icon::Appearance,
        "Keys" => Icon::Keyboard,
        _ => Icon::Gear,
    }
}

/// The title with matched characters in the accent colour.
fn highlighted(title: &str, matches: &[usize], selected: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    let base = if selected { theme::text() } else { theme::text().gamma_multiply(0.92) };
    for (i, c) in title.chars().enumerate() {
        let hit = matches.binary_search(&i).is_ok();
        let format = egui::TextFormat { font_id: FontId::proportional(13.5), color: if hit { theme::accent() } else { base }, ..Default::default() };
        job.append(c.encode_utf8(&mut [0; 4]), 0.0, format);
    }
    job
}

fn footer(ui: &mut egui::Ui) {
    ui.add_space(4.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::hover());
    let p = ui.painter();
    p.hline(rect.x_range(), rect.top(), Stroke::new(1.0, theme::border().gamma_multiply(0.6)));
    let y = rect.center().y + 2.0;
    let mut x = rect.left() + 8.0;
    for (key, what) in [("Up Down", "navigate"), ("Enter", "run"), ("Esc", "close")] {
        let g = p.layout_no_wrap(key.to_string(), FontId::monospace(10.0), theme::text_dim());
        let chip = Rect::from_min_size(Pos2::new(x, y - 8.0), Vec2::new(g.size().x + 8.0, 16.0));
        p.rect_filled(chip, CornerRadius::same(4), theme::surface_3());
        p.galley(chip.center() - g.size() / 2.0, g, theme::text_dim());
        let label = p.text(Pos2::new(chip.right() + 5.0, y), Align2::LEFT_CENTER, what, FontId::proportional(11.0), theme::text_faint());
        x = label.right() + 14.0;
    }
    p.text(
        Pos2::new(rect.right() - 8.0, y),
        Align2::RIGHT_CENTER,
        "type words in any order",
        FontId::proportional(11.0),
        theme::text_faint().gamma_multiply(0.8),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::Action;

    fn registry() -> Registry {
        let mut r = Registry::default();
        r.add(Entry::action("Pane", Action::SplitRight));
        r.add(Entry::action("Pane", Action::ZoomPane));
        r.add(Entry::new("Theme", "Nord", Command::SetTheme("Nord".into())));
        r
    }

    #[test]
    fn keyboard_selection_wraps_and_resets_on_typing() {
        let mut p = Palette::new(registry());
        assert_eq!(p.result_titles().len(), 3);
        p.step(-1);
        assert_eq!(p.selected, 2);
        p.step(1);
        assert_eq!(p.selected, 0);
        p.step(1);
        p.set_query("nord".into());
        assert_eq!(p.selected, 0);
        assert_eq!(p.result_titles(), vec!["Nord"]);
        assert!(matches!(p.chosen(), Some(Command::SetTheme(t)) if t == "Nord"));
        p.set_query("qqq".into());
        assert!(p.chosen().is_none());
        p.step(1);
    }

    #[test]
    fn renders_headless_and_runs_on_enter() {
        let ctx = egui::Context::default();
        let mut palette = Palette::new(registry()).with_query("zoom");
        let mut outcome = None;
        for (i, enter) in [false, false, true].into_iter().enumerate() {
            let mut input = egui::RawInput { time: Some(i as f64 * 0.1), ..Default::default() };
            if enter {
                input.events.push(egui::Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
            }
            let mut out = ctx.run_ui(input, |ui| {
                if let PaletteOutcome::Run(cmd) = palette.show(ui.ctx()) {
                    outcome = Some(cmd);
                }
            });
            out.textures_delta.clear();
        }
        assert!(matches!(outcome, Some(Command::App(Action::ZoomPane))), "{outcome:?}");
    }
}
