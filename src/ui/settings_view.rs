//! Settings screen: a large centred modal with a left navigation rail and
//! card-based sections. Every change is applied live and persisted at once
//! (slider drags are kept in memory and written when the drag ends).

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Id, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, UiBuilder, Vec2, emath::TSTransform};

use super::settings_sections::{self as sections, SectionState};
use super::settings_widgets::{self as w, Icon};
use crate::settings::{self, Settings};
use crate::{theme, themes};

const NAV_W: f32 = 216.0;
const NAV_ITEM_H: f32 = 38.0;
const HEADER_H: f32 = 76.0;
const RADIUS: u8 = 16;
const OPEN_SECS: f32 = 0.22;
const CLOSE_SECS: f32 = 0.16;
const SECTION_FADE_SECS: f32 = 0.2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Appearance,
    Layout,
    Terminal,
    Sftp,
    Editor,
    FileTypes,
    Keyboard,
    Agents,
    About,
}

impl Section {
    pub const ALL: [Section; 9] = [
        Section::Appearance,
        Section::Layout,
        Section::Terminal,
        Section::Sftp,
        Section::Editor,
        Section::FileTypes,
        Section::Keyboard,
        Section::Agents,
        Section::About,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Layout => "Layout",
            Section::Terminal => "Terminal",
            Section::Sftp => "SFTP",
            Section::Editor => "Editor",
            Section::FileTypes => "File types",
            Section::Keyboard => "Keyboard",
            Section::Agents => "AI agents",
            Section::About => "About",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Section::Appearance => "Themes, interface scale and motion",
            Section::Layout => "Tabs, sidebar and panes, arranged your way",
            Section::Terminal => "Typography, cursor and behaviour of every terminal",
            Section::Sftp => "How file panes look and behave",
            Section::Editor => "The built-in remote file editor",
            Section::FileTypes => "Choose what opens each kind of file",
            Section::Keyboard => "Click a shortcut, then press the new keys",
            Section::Agents => "Claude Code, Codex and other agent sessions, CPU and memory",
            Section::About => "Version, shortcuts and maintenance",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Section::Appearance => Icon::Appearance,
            Section::Layout => Icon::Layout,
            Section::Terminal => Icon::Terminal,
            Section::Sftp => Icon::Folder,
            Section::Editor => Icon::Editor,
            Section::FileTypes => Icon::FileType,
            Section::Keyboard => Icon::Keyboard,
            Section::Agents => Icon::Server,
            Section::About => Icon::Info,
        }
    }
}

pub struct SettingsView {
    section: Section,
    opened_at: Option<f64>,
    closing_at: Option<f64>,
    close_requested: bool,
    section_at: f64,
    pending_save: bool,
    state: SectionState,
}

impl Default for SettingsView {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsView {
    pub fn new() -> Self {
        Self::at(Section::Appearance)
    }

    /// Opens on a specific section.
    pub fn at(section: Section) -> Self {
        Self {
            section,
            opened_at: None,
            closing_at: None,
            close_requested: false,
            section_at: f64::NEG_INFINITY,
            pending_save: false,
            state: SectionState::default(),
        }
    }

    /// Starts the closing animation (`show` returns false once it has finished).
    pub fn close(&mut self) {
        self.close_requested = true;
    }

    pub fn is_closing(&self) -> bool {
        self.close_requested || self.closing_at.is_some()
    }

    /// Draws the screen. Returns false once it has been closed (Esc, click outside
    /// or the close button) and its fade-out has finished.
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        themes::tick(ctx);
        let now = ctx.input(|i| i.time);
        if self.opened_at.is_none() {
            // Take the keyboard away from whatever had it (usually a terminal).
            ctx.memory_mut(|m| m.stop_text_input());
        }
        let opened = *self.opened_at.get_or_insert(now);
        if self.close_requested && self.closing_at.is_none() {
            self.closing_at = Some(now);
        }
        let t = match self.closing_at {
            None => progress(now - opened, theme::anim_time(OPEN_SECS)),
            Some(c) => 1.0 - progress(now - c, theme::anim_time(CLOSE_SECS)),
        };
        if self.closing_at.is_some() && t <= 0.0 {
            self.finish(ctx);
            return false;
        }
        if t < 1.0 {
            ctx.request_repaint();
        }
        let eased = themes::ease_out_cubic(t);

        let screen = ctx.content_rect();
        let size = Vec2::new((screen.width() - 48.0).clamp(320.0, 980.0), (screen.height() - 48.0).clamp(300.0, 680.0));
        let id = Id::new("tether_settings_modal");
        let before = settings::get();
        let mut draft = before.clone();
        let modal = egui::Modal::new(id).backdrop_color(Color32::TRANSPARENT).frame(egui::Frame::NONE).area(egui::Modal::default_area(id).fade_in(false)).show(
            ctx,
            |ui| {
                ui.multiply_opacity(eased);
                let backdrop = ui.painter().add(Shape::Noop);
                let close = self.window(ui, size, &mut draft, now);
                (backdrop, close)
            },
        );
        let (backdrop, close_clicked) = modal.inner;
        if self.closing_at.is_none() && (close_clicked || modal.should_close()) {
            self.closing_at = Some(now);
        }

        // Scale the window in from 96 % around the screen centre, then paint the
        // (unscaled) backdrop into the placeholder slot below it.
        let layer = modal.response.layer_id;
        let scale = 0.96 + 0.04 * eased;
        let shift = screen.center().to_vec2() * (1.0 - scale) + Vec2::new(0.0, 10.0 * (1.0 - eased));
        ctx.transform_layer_shapes(layer, TSTransform::new(shift, scale));
        let dim = if theme::palette().dark { 150 } else { 80 };
        ctx.layer_painter(layer).set(backdrop, Shape::rect_filled(screen, 0.0, Color32::from_black_alpha(dim).gamma_multiply(eased)));

        self.commit(ctx, &before, draft);
        true
    }

    fn finish(&mut self, ctx: &egui::Context) {
        // Also drops a hover preview from the theme gallery.
        themes::apply(ctx, &settings::get().appearance.theme);
        crate::keybindings::set_recording(false);
        if self.pending_save {
            let _ = settings::save();
        }
        *self = Self::at(self.section);
    }

    /// Persists and applies whatever the frame changed.
    fn commit(&mut self, ctx: &egui::Context, before: &Settings, draft: Settings) {
        let dragging = ctx.dragged_id().is_some();
        if draft != *before {
            let theme_changed = !draft.appearance.theme.eq_ignore_ascii_case(&before.appearance.theme);
            let anim_changed = draft.appearance.animations != before.appearance.animations;
            let draft_appearance = draft.appearance.clone();
            if dragging {
                settings::preview(|s| *s = draft);
                self.pending_save = true;
            } else {
                let _ = settings::update(|s| *s = draft);
                self.pending_save = false;
            }
            let (a, b) = (&draft_appearance, &before.appearance);
            let backdrop_changed = a.window_opacity != b.window_opacity || a.background_images != b.background_images;
            if theme_changed {
                themes::apply_animated(ctx, &settings::get().appearance.theme);
            } else if anim_changed {
                theme::install(ctx);
            }
            if backdrop_changed {
                themes::refresh(ctx);
            }
            if a.terminal_font != b.terminal_font {
                crate::fonts::apply(ctx);
            }
        }
        if !dragging {
            if self.pending_save {
                let _ = settings::save();
                self.pending_save = false;
            }
            // Zoom only on release so the slider doesn't jump under the pointer.
            if (settings::get().appearance.ui_scale() - ctx.zoom_factor()).abs() > 1e-3 {
                theme::install(ctx);
            }
        }
    }

    /// The window body. Returns true when the close button was clicked.
    fn window(&mut self, ui: &mut egui::Ui, size: Vec2, s: &mut Settings, now: f64) -> bool {
        let (rect, _) = ui.allocate_exact_size(size, Sense::click());
        let p = ui.painter().clone();
        let shadow = egui::Shadow { offset: [0, 18], blur: 48, spread: 0, color: theme::shadow() };
        p.add(shadow.as_shape(rect, CornerRadius::same(RADIUS)));
        p.rect(rect, CornerRadius::same(RADIUS), theme::surface(), Stroke::new(1.0, theme::border().gamma_multiply(0.7)), StrokeKind::Inside);

        let nav = Rect::from_min_max(rect.min, Pos2::new(rect.left() + NAV_W, rect.bottom()));
        let nav_round = CornerRadius { nw: RADIUS, sw: RADIUS, ne: 0, se: 0 };
        p.rect_filled(nav.shrink(0.5), nav_round, theme::bg());
        p.vline(nav.right(), nav.y_range(), Stroke::new(1.0, theme::border().gamma_multiply(0.45)));
        self.nav(ui, nav, now);

        let content = Rect::from_min_max(Pos2::new(nav.right(), rect.top()), rect.max);
        let header = Rect::from_min_size(content.min, Vec2::new(content.width(), HEADER_H));
        p.text(header.left_top() + Vec2::new(32.0, 26.0), Align2::LEFT_TOP, self.section.title(), FontId::proportional(21.0), theme::text());
        p.text(header.left_top() + Vec2::new(32.0, 52.0), Align2::LEFT_TOP, self.section.subtitle(), FontId::proportional(12.5), theme::text_dim());
        let close_rect = Rect::from_center_size(Pos2::new(header.right() - 30.0, header.top() + 30.0), Vec2::splat(30.0));
        let close = ui.scope_builder(UiBuilder::new().max_rect(close_rect), |ui| w::close_button(ui, 30.0, false)).inner;
        let close = close.on_hover_text("Close (Esc)").clicked();

        // Stop short of the rounded bottom corners so scrolled content never pokes out.
        let body = Rect::from_min_max(Pos2::new(content.left(), header.bottom()), content.max - Vec2::new(0.0, 10.0));
        let k = themes::ease_out_cubic(progress(now - self.section_at, theme::anim_time(SECTION_FADE_SECS)));
        if k < 1.0 {
            ui.ctx().request_repaint();
        }
        let mut child = ui.new_child(UiBuilder::new().max_rect(body.translate(Vec2::new(0.0, 8.0 * (1.0 - k)))));
        child.multiply_opacity(k);
        egui::ScrollArea::vertical().id_salt(("settings_body", self.section as u8)).auto_shrink([false, false]).show(&mut child, |ui| {
            egui::Frame::new().inner_margin(egui::Margin { left: 32, right: 28, top: 4, bottom: 28 }).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                sections::show(ui, self.section, s, &mut self.state, now);
            });
        });
        close
    }

    fn nav(&mut self, ui: &mut egui::Ui, nav: Rect, now: f64) {
        let p = ui.painter().clone();
        let left = nav.left() + 16.0;
        // Brand mark: accent tile with a painted prompt chevron.
        let mark = Rect::from_min_size(Pos2::new(left + 4.0, nav.top() + 24.0), Vec2::splat(26.0));
        p.rect_filled(mark, CornerRadius::same(7), theme::accent());
        let ink = Stroke::new(2.0, theme::on_accent());
        p.line_segment([mark.left_top() + Vec2::new(8.0, 8.0), mark.left_top() + Vec2::new(13.0, 13.0)], ink);
        p.line_segment([mark.left_top() + Vec2::new(13.0, 13.0), mark.left_top() + Vec2::new(8.0, 18.0)], ink);
        p.text(Pos2::new(mark.right() + 10.0, mark.center().y), Align2::LEFT_CENTER, "Settings", FontId::proportional(16.5), theme::text());

        // Reserve a fixed footer and scroll navigation on short windows / larger UI
        // scales, so About and the version never overlap or become unreachable.
        let body = Rect::from_min_max(Pos2::new(left, nav.top() + 74.0), Pos2::new(nav.right() - 16.0, nav.bottom() - 52.0));
        let mut child = ui.new_child(UiBuilder::new().max_rect(body));
        child.set_clip_rect(body);
        egui::ScrollArea::vertical().id_salt("settings_navigation").auto_shrink([false, false]).show(&mut child, |ui| {
            let p = ui.painter().clone();
            let top = ui.cursor().top();
            ui.set_min_height(Section::ALL.len() as f32 * NAV_ITEM_H);
            let idx = Section::ALL.iter().position(|s| *s == self.section).unwrap_or(0);
            let y = ui.ctx().animate_value_with_time(ui.id().with("nav_sel"), idx as f32 * NAV_ITEM_H, theme::anim_time(0.22));
            let item_rect = |y: f32| Rect::from_min_size(Pos2::new(left, top + y), Vec2::new(NAV_W - 32.0, NAV_ITEM_H - 4.0));
            let sel = item_rect(y);
            let pill = if theme::palette().dark { theme::surface_2() } else { theme::surface() };
            p.rect_filled(sel, CornerRadius::same(9), pill);
            let bar = Rect::from_center_size(Pos2::new(sel.left() + 1.5, sel.center().y), Vec2::new(3.0, 16.0));
            p.rect_filled(bar, CornerRadius::same(2), theme::accent());

            for (i, section) in Section::ALL.into_iter().enumerate() {
                let r = item_rect(i as f32 * NAV_ITEM_H);
                let resp = ui.interact(r, ui.id().with(("nav", i)), Sense::click());
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, section.title()));
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() && i != idx, theme::anim_time(0.12));
                if hover > 0.0 {
                    p.rect_filled(r, CornerRadius::same(9), theme::surface_2().gamma_multiply(0.55 * hover));
                }
                let active = section == self.section;
                let colour = if active { theme::text() } else { theme::text_dim().lerp_to_gamma(theme::text(), hover) };
                let icon_colour = if active { theme::accent() } else { colour };
                let icon = Rect::from_center_size(Pos2::new(r.left() + 22.0, r.center().y), Vec2::splat(15.0));
                w::paint_icon(&p, icon, section.icon(), icon_colour);
                p.text(Pos2::new(r.left() + 40.0, r.center().y), Align2::LEFT_CENTER, section.title(), FontId::proportional(13.5), colour);
                if resp.clicked() && !active {
                    self.section = section;
                    self.section_at = now;
                    // A shortcut being recorded must not keep every shortcut disabled.
                    self.state.leave_section();
                }
            }
        });

        let foot = Pos2::new(left + 4.0, nav.bottom() - 22.0);
        let version = concat!("tether ", env!("CARGO_PKG_VERSION"));
        p.text(foot, Align2::LEFT_BOTTOM, version, FontId::proportional(11.0), theme::text_faint());
        p.text(foot + Vec2::new(0.0, 4.0), Align2::LEFT_TOP, "Esc to close", FontId::proportional(11.0), theme::text_faint());
    }
}

/// Linear 0..=1 progress of `elapsed` over `duration` (instant when duration is 0).
fn progress(elapsed: f64, duration: f32) -> f32 {
    if duration <= 0.0 { 1.0 } else { (elapsed as f32 / duration).clamp(0.0, 1.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_handles_zero_duration() {
        assert_eq!(progress(0.0, 0.0), 1.0);
        assert_eq!(progress(0.1, 0.2), 0.5);
        assert_eq!(progress(-1.0, 0.2), 0.0);
        assert_eq!(progress(5.0, 0.2), 1.0);
    }

    fn frame(ctx: &egui::Context, time: f64, f: &mut dyn FnMut(&egui::Context)) {
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 800.0))), time: Some(time), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| f(ui.ctx()));
        out.textures_delta.clear();
    }

    #[test]
    fn renders_every_section_headless_and_closes() {
        let ctx = egui::Context::default();
        let mut view = SettingsView::new();
        let mut time = 0.0;
        for section in Section::ALL {
            view.section = section;
            for _ in 0..3 {
                time += 0.1;
                frame(&ctx, time, &mut |ctx| assert!(view.show(ctx)));
            }
        }
        view.close();
        let mut frames = 0;
        let mut open = true;
        while open && frames < 10 {
            time += 0.1;
            frames += 1;
            frame(&ctx, time, &mut |ctx| open = view.show(ctx));
        }
        assert!(!open && frames <= 3, "fade-out should finish quickly");
        assert!(!view.is_closing(), "state resets so it can reopen");
    }

    /// Renders every section in a dark and a light theme to `target/ui-shots/`.
    /// Needs a GPU adapter: `cargo test --lib settings_screens -- --ignored`.
    #[test]
    #[ignore]
    fn settings_screens() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/ui-shots");
        std::fs::create_dir_all(&dir).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(Vec2::new(1280.0, 800.0)).wgpu().build_ui_state(
            |ui, view: &mut SettingsView| {
                ui.painter().rect_filled(ui.max_rect(), 0.0, theme::bg());
                view.show(ui.ctx());
            },
            SettingsView::new(),
        );
        for name in ["Tether Night", "Catppuccin Latte"] {
            themes::apply(&h.ctx, name);
            for section in Section::ALL {
                h.state_mut().section = section;
                for _ in 0..30 {
                    h.step();
                }
                let img = h.render().expect("render");
                let file = format!("settings-{}-{:?}.png", name.replace(' ', "-").to_lowercase(), section).to_lowercase();
                img.save(dir.join(file)).unwrap();
            }
        }
        // Gallery in a gradient theme and in a light one.
        for name in ["Synthwave '84", "GitHub Light"] {
            themes::apply(&h.ctx, name);
            h.state_mut().section = Section::Appearance;
            for _ in 0..30 {
                h.step();
            }
            let file = format!("settings-gallery-{}.png", name.replace([' ', '\''], "-").to_lowercase());
            h.render().expect("render").save(dir.join(file)).unwrap();
        }
        themes::apply(&h.ctx, themes::DEFAULT_THEME);
    }

    #[test]
    fn sections_have_titles() {
        for s in Section::ALL {
            assert!(!s.title().is_empty() && !s.subtitle().is_empty());
        }
    }
}
