//! Content of each Settings section. Every function edits a draft `Settings`;
//! `SettingsView` diffs it against the stored one and persists the change.

use eframe::egui::{self, Align, CornerRadius, FontFamily, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};

use super::settings_view::Section;
use super::settings_widgets::{self as w, SliderSpec};
use super::terminal_view::{metrics, paint_cursor};
use super::theme_gallery;
use crate::settings::{self, Appearance, CursorStyle, EditorSettings, OpenWith, Settings, TerminalSettings};
use crate::theme;

const CARD_GAP: f32 = 14.0;

/// UI state that is not a setting (text fields, confirmations, demos).
#[derive(Default)]
pub struct SectionState {
    gallery: super::theme_gallery::GalleryState,
    new_ext: String,
    new_ext_open: OpenWith,
    ext_error: Option<String>,
    confirm_reset: bool,
    bell_demo_at: Option<f64>,
    keyboard: super::settings_keyboard::KeyboardState,
    agents: super::settings_agents::AgentsState,
}

impl SectionState {
    /// Called when another section is opened.
    pub fn leave_section(&mut self) {
        self.keyboard.cancel();
    }
}

pub fn show(ui: &mut Ui, section: Section, s: &mut Settings, st: &mut SectionState, now: f64) {
    match section {
        Section::Appearance => appearance(ui, s, st),
        Section::Layout => super::layout_settings::show(ui, s, CARD_GAP),
        Section::Terminal => terminal(ui, s, st, now),
        Section::Sftp => sftp(ui, s),
        Section::Editor => editor(ui, s),
        Section::FileTypes => file_types(ui, s, st),
        Section::Keyboard => super::settings_keyboard::show(ui, s, &mut st.keyboard, CARD_GAP),
        Section::Agents => super::settings_agents::show(ui, s, &mut st.agents, CARD_GAP),
        Section::About => about(ui, s, st),
    }
}

// ---- appearance ------------------------------------------------------------------------

fn appearance(ui: &mut Ui, s: &mut Settings, st: &mut SectionState) {
    w::card(ui, Some("Theme"), |ui| theme_gallery::theme_section(ui, &mut s.appearance, &mut st.gallery));
    ui.add_space(CARD_GAP);
    w::card(ui, Some("System"), |ui| theme_gallery::sync_section(ui, &mut s.appearance));
    ui.add_space(CARD_GAP);
    w::card(ui, Some("Window and background"), |ui| theme_gallery::window_section(ui, &mut s.appearance));
    ui.add_space(CARD_GAP);
    w::card(ui, Some("Interface"), |ui| {
        let mut pct = s.appearance.ui_scale() * 100.0;
        w::row(ui, "Interface scale", "Size of all text and controls (applied when you release)", |ui| {
            let spec =
                SliderSpec { min: Appearance::SCALE_MIN * 100.0, max: Appearance::SCALE_MAX * 100.0, step: 5.0, log: false, format: &|v| format!("{v:.0}%") };
            if w::slider_with_value(ui, &mut pct, &spec).changed() {
                s.appearance.ui_scale = pct / 100.0;
            }
        });
        w::separator(ui);
        w::row(ui, "Animations", "Motion for tabs, toasts, cursor and transitions", |ui| {
            w::toggle(ui, &mut s.appearance.animations);
        });
    });
}

// ---- terminal --------------------------------------------------------------------------

fn terminal(ui: &mut Ui, s: &mut Settings, st: &mut SectionState, now: f64) {
    terminal_preview(ui, &s.terminal, st.bell_demo_at, now);
    ui.add_space(CARD_GAP);
    let (t, font) = (&mut s.terminal, &mut s.appearance.terminal_font);
    w::card(ui, Some("Text"), |ui| {
        theme_gallery::font_row(ui, font);
        w::separator(ui);
        w::row(
            ui,
            "Font size",
            &format!(
                "{} / {} / {} in a terminal",
                crate::keybindings::label(crate::keybindings::Action::FontBigger),
                crate::keybindings::label(crate::keybindings::Action::FontSmaller),
                crate::keybindings::label(crate::keybindings::Action::FontReset)
            ),
            |ui| {
                let spec =
                    SliderSpec { min: TerminalSettings::FONT_MIN, max: TerminalSettings::FONT_MAX, step: 0.5, log: false, format: &|v| format!("{v:.1} pt") };
                let mut v = t.font_size();
                if w::slider_with_value(ui, &mut v, &spec).changed() {
                    t.font_size = v;
                }
            },
        );
        w::separator(ui);
        w::row(ui, "Line height", "Extra breathing room between lines", |ui| {
            let spec = SliderSpec {
                min: TerminalSettings::LINE_HEIGHT_MIN,
                max: TerminalSettings::LINE_HEIGHT_MAX,
                step: 0.05,
                log: false,
                format: &|v| format!("{v:.2}x"),
            };
            let mut v = t.line_height();
            if w::slider_with_value(ui, &mut v, &spec).changed() {
                t.line_height = v;
            }
        });
        w::separator(ui);
        w::row(ui, "Padding", "Space between the text and the pane edge", |ui| {
            let spec = SliderSpec { min: 0.0, max: TerminalSettings::PADDING_MAX, step: 1.0, log: false, format: &|v| format!("{v:.0} px") };
            let mut v = t.padding();
            if w::slider_with_value(ui, &mut v, &spec).changed() {
                t.padding = v;
            }
        });
    });
    ui.add_space(CARD_GAP);
    w::card(ui, Some("Cursor"), |ui| {
        w::row(ui, "Style", "", |ui| {
            w::segmented(ui, &mut t.cursor_style, &[(CursorStyle::Block, "Block"), (CursorStyle::Bar, "Bar"), (CursorStyle::Underline, "Underline")]);
        });
        w::separator(ui);
        w::row(ui, "Blink", "Blinks while the terminal has focus", |ui| {
            w::toggle(ui, &mut t.cursor_blink);
        });
    });
    ui.add_space(CARD_GAP);
    w::card(ui, Some("Behaviour"), |ui| {
        w::row(ui, "Copy on select", "Selecting text copies it to the clipboard", |ui| {
            w::toggle(ui, &mut t.copy_on_select);
        });
        w::separator(ui);
        w::row(ui, "Visual bell", "A soft flash instead of a sound", |ui| {
            w::toggle(ui, &mut t.visual_bell);
            ui.add_space(6.0);
            if w::ghost_button(ui, "Try it", theme::text_dim()).clicked() {
                st.bell_demo_at = Some(now);
            }
        });
        w::separator(ui);
        w::row(ui, "Scrollbar", "Thin bar that appears while scrolling history", |ui| {
            w::toggle(ui, &mut t.scrollbar);
        });
        w::separator(ui);
        w::row(ui, "Scrollback", "Lines kept per terminal (applies to new terminals)", |ui| {
            let spec = SliderSpec {
                min: TerminalSettings::SCROLLBACK_MIN as f32,
                max: TerminalSettings::SCROLLBACK_MAX as f32,
                step: 500.0,
                log: true,
                format: &|v| if v >= 1000.0 { format!("{:.1}k", v / 1000.0).replace(".0k", "k") } else { format!("{v:.0}") },
            };
            let mut v = t.scrollback() as f32;
            if w::slider_with_value(ui, &mut v, &spec).changed() {
                t.scrollback = v as usize;
            }
        });
    });
    ui.add_space(CARD_GAP);
    w::card(ui, Some("Local shell"), |ui| {
        w::row(ui, "Default shell", "Used by “New local terminal” and the welcome screen", |ui| {
            let detected = crate::pty::shells::detected();
            let shells = detected.unwrap_or_default();
            let current = match detected {
                None => crate::pty::shells::DETECTING,
                Some(shells) => crate::pty::shells::pick_default(shells, &t.default_shell).map_or("None found", |s| s.name.as_str()),
            };
            egui::ComboBox::from_id_salt("default_shell").selected_text(current).show_ui(ui, |ui| {
                for shell in shells {
                    ui.selectable_value(&mut t.default_shell, shell.id.clone(), &shell.name);
                }
            });
        });
    });
    ui.add_space(CARD_GAP);
    super::settings_shell::card(ui, &mut s.shell_integration);
}

/// Coloured text runs for the preview: (ANSI index or `None` for the default fg, text).
const PREVIEW: &[&[(Option<usize>, &str)]] = &[
    &[(Some(2), "deploy@prod-01"), (None, ":"), (Some(4), "~/app"), (None, "$ git status --short")],
    &[(Some(3), " M "), (None, "src/server.rs")],
    &[(Some(1), " D "), (None, "config/legacy.toml")],
    &[(Some(2), "?? "), (None, "migrations/0042_index.sql")],
    &[(Some(6), "info "), (None, "3 files changed, "), (Some(2), "+18"), (None, " "), (Some(1), "-4")],
    &[(Some(2), "deploy@prod-01"), (None, ":"), (Some(4), "~/app"), (None, "$ ")],
];

fn terminal_preview(ui: &mut Ui, t: &TerminalSettings, bell_at: Option<f64>, now: f64) {
    let font = FontId::new(t.font_size(), FontFamily::Monospace);
    let m = metrics(ui.ctx(), &font, t.line_height());
    let pad = t.padding();
    let height = (PREVIEW.len() as f32 * m.cell_h + pad * 2.0).max(120.0);
    let (outer, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height + 30.0), Sense::hover());
    let p = ui.painter_at(outer);
    // Window chrome so it reads as "a terminal", not a text box.
    let bar = Rect::from_min_size(outer.min, Vec2::new(outer.width(), 30.0));
    let round_top = CornerRadius { nw: 12, ne: 12, sw: 0, se: 0 };
    p.rect_filled(bar, round_top, theme::surface_3());
    for (i, c) in [theme::danger(), theme::warn(), theme::success()].into_iter().enumerate() {
        p.circle_filled(Pos2::new(bar.left() + 16.0 + i as f32 * 14.0, bar.center().y), 4.5, c.gamma_multiply(0.9));
    }
    p.text(bar.center(), egui::Align2::CENTER_CENTER, "Live preview", FontId::proportional(11.5), theme::text_dim());
    let body = Rect::from_min_max(Pos2::new(outer.left(), bar.bottom()), outer.max);
    p.rect_filled(body, CornerRadius { nw: 0, ne: 0, sw: 12, se: 12 }, theme::term_bg());

    let inner = body.shrink(pad);
    let mut cursor = inner.min;
    for (row, runs) in PREVIEW.iter().enumerate() {
        let y = inner.top() + row as f32 * m.cell_h;
        let mut x = inner.left();
        for (colour, text) in runs.iter() {
            let c = colour.map_or(theme::term_fg(), theme::ansi);
            p.text(Pos2::new(x, y + m.text_dy), egui::Align2::LEFT_TOP, *text, font.clone(), c);
            x += text.chars().count() as f32 * m.cell_w;
        }
        cursor = Pos2::new(x, y);
    }
    let blink_on = !t.cursor_blink || (now % 1.1) < 0.66;
    if blink_on {
        let cell = Rect::from_min_size(cursor, Vec2::new(m.cell_w, m.cell_h));
        paint_cursor(&p, cell, m.text_dy, t.cursor_style, true, None, &font);
    }
    if t.cursor_blink {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
    if let Some(at) = bell_at {
        let k = 1.0 - ((now - at) / 0.45) as f32;
        if k > 0.0 && t.visual_bell {
            let k = k * k;
            let r = CornerRadius { nw: 0, ne: 0, sw: 12, se: 12 };
            p.rect_filled(body, r, theme::term_fg().gamma_multiply(0.06 * k));
            p.rect_stroke(body.shrink(0.5), r, Stroke::new(1.5, theme::warn().gamma_multiply(0.7 * k)), egui::StrokeKind::Inside);
            ui.ctx().request_repaint();
        }
    }
    p.rect_stroke(outer, CornerRadius::same(12), Stroke::new(1.0, theme::border().gamma_multiply(0.6)), egui::StrokeKind::Inside);
}

// ---- sftp / editor ---------------------------------------------------------------------

fn sftp(ui: &mut Ui, s: &mut Settings) {
    let f = &mut s.sftp;
    w::card(ui, Some("Browsing"), |ui| {
        w::row(ui, "Show hidden files", "List dot-files in local and remote panes", |ui| {
            w::toggle(ui, &mut f.show_hidden);
        });
        w::separator(ui);
        w::row(ui, "Open with a local pane", "New SFTP tabs start split: local on the left, remote on the right", |ui| {
            w::toggle(ui, &mut f.start_with_local_pane);
        });
        w::separator(ui);
        w::row(ui, "Resume where you left off", "Reopen each host in the remote folder of its last SFTP session", |ui| {
            w::toggle(ui, &mut f.resume_last_dir);
        });
    });
    ui.add_space(CARD_GAP);
    w::card(ui, Some("Safety"), |ui| {
        w::row(ui, "Confirm before deleting", "Ask before removing files or folders", |ui| {
            w::toggle(ui, &mut f.confirm_delete);
        });
    });
}

fn editor(ui: &mut Ui, s: &mut Settings) {
    let e = &mut s.editor;
    w::card(ui, Some("Text"), |ui| {
        w::row(ui, "Font size", "", |ui| {
            let spec = SliderSpec { min: EditorSettings::FONT_MIN, max: EditorSettings::FONT_MAX, step: 0.5, log: false, format: &|v| format!("{v:.1} pt") };
            let mut v = if e.font_size.is_finite() { e.font_size } else { EditorSettings::default().font_size };
            if w::slider_with_value(ui, &mut v, &spec).changed() {
                e.font_size = v;
            }
        });
        w::separator(ui);
        w::row(ui, "Tab size", "Spaces per indentation level", |ui| {
            w::segmented(ui, &mut e.tab_size, &[(2, "2"), (4, "4"), (8, "8")]);
        });
    });
    ui.add_space(CARD_GAP);
    w::card(ui, Some("Layout"), |ui| {
        w::row(ui, "Word wrap", "Wrap long lines to the window width", |ui| {
            w::toggle(ui, &mut e.word_wrap);
        });
        w::separator(ui);
        w::row(ui, "Line numbers", "Show a gutter with line numbers", |ui| {
            w::toggle(ui, &mut e.line_numbers);
        });
    });
}

// ---- file types ------------------------------------------------------------------------

const OPEN_OPTIONS: [(OpenWith, &str); 2] = [(OpenWith::Editor, "Editor"), (OpenWith::System, "System app")];

fn file_types(ui: &mut Ui, s: &mut Settings, st: &mut SectionState) {
    w::card(ui, Some("Default"), |ui| {
        w::row(ui, "Open files with", "Used for every extension not listed below", |ui| {
            w::segmented(ui, &mut s.default_open, &OPEN_OPTIONS);
        });
    });
    ui.add_space(CARD_GAP);
    w::card(ui, Some("Per extension"), |ui| {
        add_extension_row(ui, s, st);
        if let Some(err) = &st.ext_error {
            ui.label(RichText::new(err).size(11.5).color(theme::danger()));
        }
        ui.add_space(6.0);
        if s.open_with.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new("No overrides yet. Every file opens with the default above.").size(12.0).color(theme::text_faint()));
            return;
        }
        let mut remove = None;
        for (ext, how) in s.open_with.iter_mut() {
            w::separator(ui);
            ui.horizontal(|ui| {
                ui.set_min_height(34.0);
                ext_badge(ui, ext);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if w::close_button(ui, 26.0, true).on_hover_text("Remove").clicked() {
                        remove = Some(ext.clone());
                    }
                    ui.add_space(8.0);
                    w::segmented(ui, how, &OPEN_OPTIONS);
                });
            });
        }
        if let Some(ext) = remove {
            s.open_with.remove(&ext);
        }
    });
}

fn add_extension_row(ui: &mut Ui, s: &mut Settings, st: &mut SectionState) {
    ui.horizontal(|ui| {
        let edit = egui::TextEdit::singleline(&mut st.new_ext).hint_text("Extension, e.g. log").desired_width(170.0).margin(Vec2::new(10.0, 6.0));
        let resp = ui.add(edit);
        if resp.changed() {
            st.ext_error = None;
        }
        let submitted = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.add_space(4.0);
        w::segmented(ui, &mut st.new_ext_open, &OPEN_OPTIONS);
        ui.add_space(4.0);
        if w::primary_button(ui, "Add").clicked() || submitted {
            match settings::normalize_extension(&st.new_ext) {
                None => st.ext_error = Some("Use letters, digits, '-', '_' or '.' (up to 16 characters).".into()),
                Some(ext) => {
                    s.open_with.insert(ext, st.new_ext_open);
                    st.new_ext.clear();
                    st.ext_error = None;
                }
            }
        }
    });
}

fn ext_badge(ui: &mut Ui, ext: &str) {
    let font = FontId::monospace(12.0);
    let galley = ui.painter().layout_no_wrap(format!(".{ext}"), font, theme::text());
    let (rect, _) = ui.allocate_exact_size(galley.size() + Vec2::new(18.0, 10.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(7), theme::accent_soft());
    ui.painter().galley(rect.min + Vec2::new(9.0, 5.0), galley, theme::text());
}

// ---- about -----------------------------------------------------------------------------

/// Shortcuts shown on the About page (all of them are listed under Keyboard).
const ABOUT_SHORTCUTS: &[crate::keybindings::Action] = {
    use crate::keybindings::Action as A;
    &[A::OpenSettings, A::SplitRight, A::SplitDown, A::OpenSftp, A::ToggleBroadcast, A::ToggleSidebar, A::KeyManager]
};

fn about(ui: &mut Ui, s: &mut Settings, st: &mut SectionState) {
    w::card(ui, None, |ui| {
        ui.horizontal(|ui| {
            let (mark, _) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::hover());
            let p = ui.painter();
            p.rect_filled(mark, CornerRadius::same(11), theme::accent());
            let ink = Stroke::new(3.0, theme::on_accent());
            p.line_segment([mark.left_top() + Vec2::new(13.0, 14.0), mark.left_top() + Vec2::new(21.0, 22.0)], ink);
            p.line_segment([mark.left_top() + Vec2::new(21.0, 22.0), mark.left_top() + Vec2::new(13.0, 30.0)], ink);
            p.line_segment([mark.left_top() + Vec2::new(24.0, 31.0), mark.left_top() + Vec2::new(32.0, 31.0)], ink);
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("tether").size(20.0).strong());
                ui.label(
                    RichText::new(format!("Version {}  ·  SSH terminals and pipelined SFTP", env!("CARGO_PKG_VERSION"))).size(12.0).color(theme::text_dim()),
                );
            });
        });
        if let Some(path) = settings::path() {
            w::separator(ui);
            w::row(ui, "Settings file", "", |ui| {
                let shown = path.display().to_string();
                if w::ghost_button(ui, "Copy path", theme::text_dim()).clicked() {
                    ui.ctx().copy_text(shown.clone());
                }
                ui.add(egui::Label::new(RichText::new(shown).monospace().size(11.0).color(theme::text_faint())).truncate());
            });
        }
    });
    ui.add_space(CARD_GAP);
    super::update_banner::about_card(ui, s);
    ui.add_space(CARD_GAP);
    w::card(ui, Some("Keyboard shortcuts"), |ui| {
        ui.label(RichText::new("Change any of them under Keyboard.").size(11.5).color(theme::text_dim()));
        for (i, action) in ABOUT_SHORTCUTS.iter().enumerate() {
            if i > 0 {
                w::separator(ui);
            }
            let keys = crate::keybindings::binding(*action).map(|b| b.caps()).unwrap_or_default();
            ui.horizontal(|ui| {
                ui.set_min_height(26.0);
                ui.label(RichText::new(action.title()).size(13.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    for (j, k) in keys.iter().rev().enumerate() {
                        if j > 0 {
                            ui.label(RichText::new("+").size(11.0).color(theme::text_faint()));
                        }
                        w::key_chip(ui, k);
                    }
                });
            });
        }
    });
    ui.add_space(CARD_GAP);
    w::card(ui, Some("Maintenance"), |ui| {
        let desc = if st.confirm_reset { "Click again to confirm. Hosts and workspaces are kept." } else { "Restore every setting to its default" };
        w::row(ui, "Reset settings", desc, |ui| {
            let (label, colour) = if st.confirm_reset { ("Confirm reset", theme::danger()) } else { ("Reset", theme::text()) };
            if w::ghost_button(ui, label, colour).clicked() {
                if st.confirm_reset {
                    *s = Settings::default();
                }
                st.confirm_reset = !st.confirm_reset;
            }
        });
    });
}
