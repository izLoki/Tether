//! Appearance > Theme: a searchable gallery of preview cards grouped Dark / Light /
//! Custom, with live preview on hover (reverted on leave, committed on click), user
//! theme import, "Sync with OS", window opacity, background images and the
//! terminal font picker.

use eframe::egui::{self, Align, CornerRadius, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};

use super::settings_widgets::{self as w, Icon, SliderSpec};
use super::theme_preview;
use crate::settings::{Appearance, BackgroundImage, FontChoice};
use crate::{theme, themes};

const CARD_MIN_W: f32 = 176.0;
const GRID_GAP: f32 = 12.0;
/// Hovering a card this long previews it (avoids flicker while sweeping across the grid).
const HOVER_PREVIEW_SECS: f64 = 0.18;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    All,
    Dark,
    Light,
    Custom,
}

#[derive(Default)]
pub struct GalleryState {
    tab: Tab,
    query: String,
    /// Card under the pointer and since when.
    hover: Option<(String, f64)>,
    /// Theme currently shown as a hover preview (`None` = the saved one).
    previewing: Option<String>,
    /// Result of the last import (text, is_error).
    message: Option<(String, bool)>,
}

/// Which themes the current tab and search text show.
fn matches(t: &themes::Theme, tab: Tab, query: &str) -> bool {
    let tab_ok = match tab {
        Tab::All => true,
        Tab::Dark => t.palette.dark && !t.is_custom(),
        Tab::Light => !t.palette.dark && !t.is_custom(),
        Tab::Custom => t.is_custom(),
    };
    let q = query.trim().to_lowercase();
    tab_ok && (q.is_empty() || t.name.to_lowercase().contains(&q) || t.tagline.to_lowercase().contains(&q))
}

/// The Theme card body.
pub fn theme_section(ui: &mut Ui, a: &mut Appearance, st: &mut GalleryState) {
    let ctx = ui.ctx().clone();
    themes::poll_user_themes(&ctx);
    let now = ctx.input(|i| i.time);
    ui.horizontal(|ui| {
        let search = egui::TextEdit::singleline(&mut st.query).hint_text("Search themes").desired_width(220.0).margin(Vec2::new(10.0, 6.0));
        ui.add(search);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            w::segmented(ui, &mut st.tab, &[(Tab::All, "All"), (Tab::Dark, "Dark"), (Tab::Light, "Light"), (Tab::Custom, "Custom")]);
        });
    });
    ui.add_space(10.0);

    let all = themes::all();
    let groups: Vec<(&str, Vec<&themes::Theme>)> = [("Dark", Tab::Dark), ("Light", Tab::Light), ("Custom", Tab::Custom)]
        .into_iter()
        .filter(|(_, tab)| st.tab == Tab::All || st.tab == *tab)
        .map(|(label, tab)| (label, all.iter().filter(|t| matches(t, tab, &st.query)).collect::<Vec<_>>()))
        .collect();
    let mut hovered = None;
    let mut clicked = None;
    let mut shown = 0;
    for (label, list) in &groups {
        if list.is_empty() {
            continue;
        }
        shown += list.len();
        if st.tab == Tab::All {
            ui.label(RichText::new(format!("{label}  ·  {}", list.len())).size(11.0).strong().color(theme::text_faint()));
            ui.add_space(4.0);
        }
        let (h, c) = grid(ui, list, &a.theme);
        hovered = hovered.or(h);
        clicked = clicked.or(c);
        ui.add_space(6.0);
    }
    if shown == 0 {
        let msg = if st.tab == Tab::Custom && st.query.trim().is_empty() {
            "No custom themes yet. Import a Warp theme (.yaml) or drop files into the themes folder."
        } else {
            "No theme matches your search."
        };
        ui.add_space(8.0);
        ui.label(RichText::new(msg).size(12.0).color(theme::text_faint()));
        ui.add_space(8.0);
    }
    if let Some(name) = clicked {
        choose(a, &name, ctx.system_theme());
        st.previewing = None;
        st.hover = Some((name, f64::NEG_INFINITY));
    }
    hover_preview(&ctx, st, hovered, &a.theme, now);

    w::separator(ui);
    user_theme_row(ui, a, st);
}

/// Live preview: show the hovered theme after a short delay, revert when the pointer leaves.
fn hover_preview(ctx: &egui::Context, st: &mut GalleryState, hovered: Option<String>, committed: &str, now: f64) {
    match (&hovered, &st.hover) {
        (Some(h), Some((cur, _))) if cur == h => {}
        (Some(h), _) => st.hover = Some((h.clone(), now)),
        (None, _) => st.hover = None,
    }
    let wanted = match &st.hover {
        Some((name, since)) if now - since >= HOVER_PREVIEW_SECS => Some(name.clone()),
        Some((name, since)) => {
            let same = st.previewing.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(name));
            if !same {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64((HOVER_PREVIEW_SECS - (now - since)).max(0.01)));
            }
            st.previewing.clone()
        }
        None => None,
    };
    let target = wanted.unwrap_or_else(|| committed.to_string());
    let showing = st.previewing.clone().unwrap_or_else(|| committed.to_string());
    if !target.eq_ignore_ascii_case(&showing) {
        themes::apply_animated(ctx, &target);
        st.previewing = (!target.eq_ignore_ascii_case(committed)).then_some(target);
    }
}

/// Clicking a card: with "Sync with OS" the theme fills the slot matching its brightness.
fn choose(a: &mut Appearance, name: &str, system: Option<egui::Theme>) {
    let Some(t) = themes::by_name(name) else { return };
    if a.sync_with_os {
        let slot = if t.palette.dark { egui::Theme::Dark } else { egui::Theme::Light };
        if slot == egui::Theme::Dark {
            a.dark_theme = t.name.clone()
        } else {
            a.light_theme = t.name.clone()
        }
        if system.is_some_and(|s| s != slot) {
            return;
        }
    }
    a.theme = t.name;
}

/// Lays out cards in rows. Returns (hovered, clicked) theme names.
fn grid(ui: &mut Ui, list: &[&themes::Theme], current: &str) -> (Option<String>, Option<String>) {
    let width = ui.available_width();
    let cols = (((width + GRID_GAP) / (CARD_MIN_W + GRID_GAP)).floor() as usize).max(2);
    let card_w = (width - GRID_GAP * (cols - 1) as f32) / cols as f32;
    let size = Vec2::new(card_w, card_w * 0.64 + 42.0);
    let (mut hovered, mut clicked) = (None, None);
    for chunk in list.chunks(cols) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GRID_GAP;
            for t in chunk {
                let resp = card(ui, t, size, current.eq_ignore_ascii_case(&t.name));
                if resp.hovered() {
                    hovered = Some(t.name.clone());
                }
                if resp.clicked() {
                    clicked = Some(t.name.clone());
                }
            }
        });
        ui.add_space(GRID_GAP - 6.0);
    }
    (hovered, clicked)
}

/// One theme tile with a mini-preview of its own colours.
fn card(ui: &mut Ui, t: &themes::Theme, size: Vec2, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let resp = resp.on_hover_text(&t.tagline);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), theme::anim_time(0.14));
    let sel = ui.ctx().animate_bool_with_time(resp.id.with("sel"), selected, theme::anim_time(0.2));
    let r = rect.translate(Vec2::new(0.0, -2.0 * h));
    let p = ui.painter();
    if h > 0.0 {
        p.add(egui::Shadow { offset: [0, 8], blur: 20, spread: 0, color: theme::shadow().gamma_multiply(0.6 * h) }.as_shape(r, CornerRadius::same(12)));
    }
    p.rect_filled(r, CornerRadius::same(12), theme::surface());
    let preview = Rect::from_min_size(r.min + Vec2::splat(6.0), Vec2::new(r.width() - 12.0, r.height() - 48.0));
    theme_preview::paint(&p.with_clip_rect(preview), preview, &t.palette, t.gradient);

    let name_pos = Pos2::new(r.left() + 12.0, preview.bottom() + 21.0);
    let tag = if t.is_custom() {
        "Custom"
    } else if t.palette.dark {
        "Dark"
    } else {
        "Light"
    };
    let tag_galley = p.layout_no_wrap(tag.to_string(), FontId::proportional(10.5), theme::text_faint());
    let tag_galley_w = tag_galley.size().x;
    let name_w = r.width() - 52.0 - tag_galley.size().x;
    let name = p.layout(t.name.clone(), FontId::proportional(12.5), theme::text(), name_w);
    let name_row = name.rows.first().map_or(0.0, |row| row.height());
    p.galley(Pos2::new(name_pos.x, name_pos.y - name_row / 2.0), name, theme::text());
    p.galley(Pos2::new(r.right() - 12.0 - tag_galley.size().x, name_pos.y - tag_galley.size().y / 2.0), tag_galley, theme::text_faint());
    // Accent swatch left of the tag (gradient themes show both stops).
    let sw = Pos2::new(r.right() - 20.0 - tag_galley_w, name_pos.y);
    let accent_end = t.accent_gradient.map_or(t.palette.accent, |g| g.end);
    p.circle_filled(sw - Vec2::new(7.0, 0.0), 4.0, t.palette.accent);
    p.circle_filled(sw, 4.0, accent_end);

    let ring = theme::border().gamma_multiply(0.6 + 0.4 * h).lerp_to_gamma(theme::accent(), sel);
    p.rect_stroke(r, CornerRadius::same(12), Stroke::new(1.0 + sel, ring), egui::StrokeKind::Inside);
    if sel > 0.0 {
        let c = Pos2::new(preview.right() - 12.0, preview.top() + 12.0);
        p.circle_filled(c, 9.0 * sel, theme::accent());
        w::paint_icon(p, Rect::from_center_size(c, Vec2::splat(9.0 * sel)), Icon::Check, theme::on_accent());
    }
    resp
}

/// Import / open-folder buttons and the result of the last import or reload.
fn user_theme_row(ui: &mut Ui, a: &mut Appearance, st: &mut GalleryState) {
    let errors = themes::load_errors();
    let desc = match &st.message {
        Some((m, _)) => m.clone(),
        None => "Warp themes (.yaml) or JSON with the same fields, from the themes folder".to_string(),
    };
    w::row(ui, "Custom themes", &desc, |ui| {
        if w::ghost_button(ui, "Open themes folder", theme::text()).clicked() {
            st.message = open_themes_folder().err().map(|e| (format!("Could not open the folder: {e:#}"), true));
        }
        ui.add_space(4.0);
        if w::primary_button(ui, "Import theme…").clicked() {
            import(a, st);
        }
    });
    if let Some((m, true)) = &st.message {
        ui.label(RichText::new(m).size(11.5).color(theme::danger()));
    }
    if !errors.is_empty() {
        let label = format!("{} theme file{} skipped (hover for details)", errors.len(), if errors.len() == 1 { "" } else { "s" });
        ui.label(RichText::new(label).size(11.5).color(theme::warn())).on_hover_text(errors.join("\n"));
    }
}

fn import(a: &mut Appearance, st: &mut GalleryState) {
    let Some(file) = rfd::FileDialog::new().set_title("Import theme").add_filter("Theme", &["yaml", "yml", "json"]).pick_file() else { return };
    match themes::import_file(&file) {
        Ok(name) => {
            st.message = Some((format!("Imported \"{name}\"."), false));
            a.theme = name;
            st.tab = Tab::Custom;
        }
        Err(e) => {
            eprintln!("tether: theme import {}: {e:#}", file.display());
            st.message = Some((format!("Not a valid theme: {e:#}"), true));
        }
    }
}

fn open_themes_folder() -> anyhow::Result<()> {
    let dir = themes::user_dir().ok_or_else(|| anyhow::anyhow!("no config folder"))?;
    std::fs::create_dir_all(&dir)?;
    let program = if cfg!(windows) {
        "explorer.exe"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program).arg(&dir).spawn()?;
    Ok(())
}

// ---- sync with OS ----------------------------------------------------------------------

/// "Sync with OS" toggle with a light and a dark theme slot.
pub fn sync_section(ui: &mut Ui, a: &mut Appearance) {
    let system = ui.ctx().system_theme();
    let mode = match system {
        Some(egui::Theme::Dark) => "Your system is in dark mode",
        Some(egui::Theme::Light) => "Your system is in light mode",
        None => "The system mode is unknown",
    };
    let before = (a.sync_with_os, a.light_theme.clone(), a.dark_theme.clone());
    w::row(ui, "Sync with OS", &format!("Switch between a light and a dark theme with the system. {mode}."), |ui| {
        w::toggle(ui, &mut a.sync_with_os);
    });
    if a.sync_with_os {
        let all = themes::all();
        for (label, dark) in [("Light theme", false), ("Dark theme", true)] {
            w::separator(ui);
            let slot = if dark { &mut a.dark_theme } else { &mut a.light_theme };
            w::row(ui, label, "", |ui| {
                egui::ComboBox::from_id_salt(("sync_slot", dark)).selected_text(slot.as_str()).width(210.0).height(320.0).show_ui(ui, |ui| {
                    for t in all.iter().filter(|t| t.palette.dark == dark) {
                        ui.selectable_value(slot, t.name.clone(), &t.name);
                    }
                });
            });
        }
    }
    if before != (a.sync_with_os, a.light_theme.clone(), a.dark_theme.clone())
        && let Some(name) = themes::synced_theme(a, system)
    {
        a.theme = name;
    }
}

// ---- window & background ---------------------------------------------------------------

pub fn window_section(ui: &mut Ui, a: &mut Appearance) {
    let mut pct = a.window_opacity() * 100.0;
    w::row(ui, "Window opacity", "Lets the desktop show through the window and terminal backgrounds", |ui| {
        let spec = SliderSpec { min: Appearance::OPACITY_MIN * 100.0, max: 100.0, step: 1.0, log: false, format: &|v| format!("{v:.0}%") };
        if w::slider_with_value(ui, &mut pct, &spec).changed() {
            a.window_opacity = pct / 100.0;
        }
    });
    w::separator(ui);
    let theme_name = a.theme.clone();
    let key = theme_name.trim().to_lowercase();
    let own = themes::by_name(&theme_name).and_then(|t| t.background_image);
    let chosen = a.background_image(&theme_name).cloned();
    let shown = chosen.clone().or(own.clone());
    let desc = match (&chosen, &own) {
        (Some(b), _) => format!("{} (for {theme_name})", file_name(&b.path)),
        (None, Some(b)) => format!("{} (from the theme)", file_name(&b.path)),
        (None, None) => format!("Painted behind the terminal text while {theme_name} is active"),
    };
    w::row(ui, "Background image", &desc, |ui| {
        if chosen.is_some() && w::ghost_button(ui, "Remove", theme::text_dim()).clicked() {
            a.background_images.remove(&key);
        }
        ui.add_space(4.0);
        if w::ghost_button(ui, "Choose image…", theme::text()).clicked()
            && let Some(file) = rfd::FileDialog::new().set_title("Background image").add_filter("Image", &["png", "jpg", "jpeg"]).pick_file()
        {
            let opacity = shown.as_ref().map_or(0.25, BackgroundImage::opacity);
            a.background_images.insert(key.clone(), BackgroundImage { path: file.display().to_string(), opacity });
        }
    });
    if let Some(b) = shown {
        w::separator(ui);
        let mut pct = b.opacity() * 100.0;
        w::row(ui, "Image opacity", "How strongly the image shows through", |ui| {
            let spec = SliderSpec { min: 0.0, max: 100.0, step: 1.0, log: false, format: &|v| format!("{v:.0}%") };
            if w::slider_with_value(ui, &mut pct, &spec).changed() {
                a.background_images.insert(key.clone(), BackgroundImage { path: b.path.clone(), opacity: pct / 100.0 });
            }
        });
    }
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}

// ---- terminal font ---------------------------------------------------------------------

/// Font family picker for the Terminal section (system monospace fonts).
pub fn font_row(ui: &mut Ui, choice: &mut Option<FontChoice>) {
    let fonts = crate::fonts::system_monospace_fonts(ui.ctx());
    let desc = match &fonts {
        None => "Looking for monospace fonts…".to_string(),
        Some(list) => format!("{} monospace fonts installed", list.len()),
    };
    w::row(ui, "Font family", &desc, |ui| {
        let current = choice.as_ref().map_or("Default (Hack)".to_string(), |c| c.family.clone());
        egui::ComboBox::from_id_salt("terminal_font").selected_text(current).width(210.0).height(360.0).show_ui(ui, |ui| {
            if ui.selectable_label(choice.is_none(), "Default (Hack)").clicked() {
                *choice = None;
            }
            for f in fonts.iter().flat_map(|l| l.iter()) {
                let path = f.path.display().to_string();
                let on = choice.as_ref().is_some_and(|c| c.path == path);
                if ui.selectable_label(on, &f.family).clicked() && !on {
                    *choice = Some(FontChoice { index: f.index, family: f.family.clone(), path });
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_and_search_filter_themes() {
        let all = themes::all();
        let nord = all.iter().find(|t| t.name == "Nord").unwrap();
        assert!(matches(nord, Tab::All, ""));
        assert!(matches(nord, Tab::Dark, "NOR"));
        assert!(!matches(nord, Tab::Light, ""));
        assert!(!matches(nord, Tab::Custom, ""));
        assert!(matches(nord, Tab::All, "arctic"), "tagline matches too");
        assert!(!matches(nord, Tab::All, "zzz"));
    }

    /// Gradient + custom cards and the sync/window cards, to `target/ui-shots/`.
    /// Needs a GPU adapter: `cargo test --lib gallery_extras_screen -- --ignored`.
    #[test]
    #[ignore]
    fn gallery_extras_screen() {
        let neon = "name: Neon Night\naccent: { left: '#ff2bd6', right: '#00e5ff' }\nbackground: { top: '#0b0221', bottom: '#1a0638' }\nforeground: '#e6e6ff'\ndetails: darker\n";
        let mut custom = crate::theme_import::parse(neon, false, None).unwrap();
        custom.source = themes::Source::User("neon.yaml".into());
        let list: Vec<themes::Theme> =
            ["Synthwave '84", "Horizon", "Gruvbox Light"].iter().filter_map(|n| themes::by_name(n)).chain(std::iter::once(custom)).collect();
        let mut a = Appearance { sync_with_os: true, window_opacity: 0.85, ..Default::default() };
        a.background_images.insert("tether night".into(), BackgroundImage { path: "C:/wallpapers/aurora.jpg".into(), opacity: 0.3 });
        let mut h = egui_kittest::Harness::builder().with_size(Vec2::new(900.0, 860.0)).wgpu().build_ui(move |ui| {
            ui.painter().rect_filled(ui.max_rect(), 0.0, theme::surface());
            egui::Frame::new().inner_margin(16).show(ui, |ui| {
                let refs: Vec<&themes::Theme> = list.iter().collect();
                grid(ui, &refs, "Horizon");
                w::card(ui, Some("System"), |ui| sync_section(ui, &mut a));
                ui.add_space(12.0);
                w::card(ui, Some("Window and background"), |ui| window_section(ui, &mut a));
            });
        });
        themes::apply(&h.ctx, themes::DEFAULT_THEME);
        for _ in 0..20 {
            h.step();
        }
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/ui-shots");
        std::fs::create_dir_all(&dir).unwrap();
        h.render().expect("render").save(dir.join("gallery-extras.png")).unwrap();
    }

    #[test]
    fn choosing_with_sync_fills_the_matching_slot() {
        let mut a = Appearance { sync_with_os: true, ..Default::default() };
        choose(&mut a, "gruvbox light", Some(egui::Theme::Dark));
        assert_eq!(a.light_theme, "Gruvbox Light");
        assert_eq!(a.theme, "Tether Night", "the OS is dark, so the active theme stays");
        choose(&mut a, "Nord", Some(egui::Theme::Dark));
        assert_eq!((a.dark_theme.as_str(), a.theme.as_str()), ("Nord", "Nord"));
        a.sync_with_os = false;
        choose(&mut a, "GitHub Light", Some(egui::Theme::Dark));
        assert_eq!(a.theme, "GitHub Light");
    }
}
