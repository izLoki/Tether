//! Visual identity: deep navy surfaces, one green accent,
//! soft corners, generous spacing.

use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Visuals};
use egui_dock::Style as DockStyle;

/// Every colour the UI and terminal draw with. The active palette is global so a
/// theme switch repaints everything on the next frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub surface: Color32,
    pub surface_2: Color32,
    pub surface_3: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub blue: Color32,
    pub danger: Color32,
    pub warn: Color32,
    pub success: Color32,
    /// Colour of drop shadows under windows and popups (alpha matters).
    pub shadow: Color32,
    /// Text drawn on a solid `accent` fill.
    pub on_accent: Color32,
    pub icon_page: Color32,
    pub icon_page_edge: Color32,
    pub row_separator: Color32,
    pub term_bg: Color32,
    pub term_fg: Color32,
    pub term_cursor: Color32,
    pub term_selection: Color32,
    pub ansi: [Color32; 16],
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

impl Palette {
    /// "Tether Night": the default dark palette.
    pub const TETHER_NIGHT: Palette = Palette {
        dark: true,
        bg: rgb(0x0f, 0x11, 0x1f),
        surface: rgb(0x14, 0x17, 0x29),
        surface_2: rgb(0x1b, 0x1f, 0x35),
        surface_3: rgb(0x25, 0x2a, 0x45),
        border: rgb(0x2a, 0x2f, 0x4a),
        text: rgb(0xe2, 0xe3, 0xe8),
        text_dim: rgb(0x8d, 0x91, 0xa5),
        text_faint: rgb(0x5a, 0x5f, 0x7a),
        accent: rgb(0x17, 0x87, 0x4f),
        accent_soft: Color32::from_rgba_premultiplied(0x05, 0x1b, 0x10, 0x33),
        blue: rgb(0x44, 0xa7, 0xff),
        danger: rgb(0xf2, 0x4e, 0x50),
        warn: rgb(0xfd, 0xc4, 0x7d),
        success: rgb(0x21, 0xb5, 0x68),
        shadow: Color32::from_black_alpha(140),
        on_accent: Color32::WHITE,
        icon_page: rgb(0x3a, 0x40, 0x5e),
        icon_page_edge: rgb(0x56, 0x5d, 0x80),
        row_separator: Color32::from_rgba_premultiplied(0x10, 0x12, 0x1c, 0x60),
        term_bg: rgb(0x14, 0x17, 0x29),
        term_fg: rgb(0xd4, 0xd6, 0xe0),
        term_cursor: rgb(0x21, 0xb5, 0x68),
        term_selection: Color32::from_rgba_premultiplied(0x10, 0x5a, 0x34, 0x80),
        ansi: [
            rgb(0x34, 0x38, 0x51),
            rgb(0xf2, 0x4e, 0x50),
            rgb(0x00, 0x84, 0x63),
            rgb(0xec, 0xa8, 0x55),
            rgb(0x08, 0x63, 0x9f),
            rgb(0xc1, 0x32, 0x82),
            rgb(0x20, 0x91, 0xf6),
            rgb(0xe2, 0xe3, 0xe8),
            rgb(0x8d, 0x91, 0xa5),
            rgb(0xff, 0x73, 0x75),
            rgb(0x3e, 0xd7, 0xbe),
            rgb(0xfd, 0xc4, 0x7d),
            rgb(0x6b, 0xa0, 0xc3),
            rgb(0xff, 0x7d, 0xc5),
            rgb(0x44, 0xa7, 0xff),
            rgb(0xff, 0xff, 0xff),
        ],
    };
}

static CURRENT: RwLock<Palette> = RwLock::new(Palette::TETHER_NIGHT);
/// Bumped whenever the active palette actually changes (cache key for painted colours).
static VERSION: AtomicU64 = AtomicU64::new(0);

/// A copy of the active palette. Hot loops (the terminal grid) take one per frame and
/// pass `&Palette` down instead of calling the per-colour accessors per cell.
pub fn palette() -> Palette {
    *CURRENT.read().unwrap_or_else(|e| e.into_inner())
}

/// Changes whenever [`set_palette`] installs a different palette.
pub fn palette_version() -> u64 {
    VERSION.load(Ordering::Acquire)
}

/// Swaps the active palette; call `install` afterwards to refresh egui's visuals.
pub fn set_palette(p: Palette) {
    let mut current = CURRENT.write().unwrap_or_else(|e| e.into_inner());
    if *current != p {
        *current = p;
        VERSION.fetch_add(1, Ordering::AcqRel);
    }
}

macro_rules! colour_accessors {
    ($($name:ident),* $(,)?) => {
        $(
            #[inline]
            pub fn $name() -> Color32 {
                palette().$name
            }
        )*
    };
}

colour_accessors!(
    bg,
    surface,
    surface_2,
    surface_3,
    border,
    text,
    text_dim,
    text_faint,
    accent,
    accent_soft,
    blue,
    danger,
    warn,
    success,
    shadow,
    on_accent,
    icon_page,
    icon_page_edge,
    row_separator,
    term_bg,
    term_fg,
    term_cursor,
    term_selection,
);

pub fn ansi(i: usize) -> Color32 {
    palette().ansi[i & 15]
}

/// File-category tints used by the painted file icons and their extension badges.
pub const CAT_CODE: Color32 = Color32::from_rgb(0x3e, 0xd5, 0x8a);
pub const CAT_IMAGE: Color32 = Color32::from_rgb(0xff, 0x7d, 0xc5);
pub const CAT_ARCHIVE: Color32 = Color32::from_rgb(0xff, 0xa2, 0x4c);
pub const CAT_MEDIA: Color32 = Color32::from_rgb(0xb3, 0x8b, 0xff);
pub const CAT_TEXT: Color32 = Color32::from_rgb(0xb4, 0xb8, 0xcc);
pub const CAT_CONFIG: Color32 = Color32::from_rgb(0x3e, 0xd7, 0xbe);
pub const CAT_EXEC: Color32 = Color32::from_rgb(0xff, 0x73, 0x75);
pub const CAT_DOC: Color32 = Color32::from_rgb(0x6b, 0xa0, 0xe3);
pub const RADIUS: u8 = 8;
pub const RADIUS_SM: u8 = 5;

pub fn hue_color(hue: f32) -> Color32 {
    egui::ecolor::Hsva::new(hue.rem_euclid(360.0) / 360.0, 0.55, 0.85, 1.0).into()
}

/// xterm 256-color lookup in the active palette.
pub fn indexed(i: u8) -> Color32 {
    indexed_in(&palette(), i)
}

/// xterm 256-color lookup in `p` (no global access).
pub fn indexed_in(p: &Palette, i: u8) -> Color32 {
    match i {
        0..=15 => p.ansi[i as usize],
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        232..=255 => {
            let v = 8 + (i - 232) * 10;
            Color32::from_rgb(v, v, v)
        }
    }
}

/// Animation duration scaled by the "animations" setting (0 when they are off).
pub fn anim_time(seconds: f32) -> f32 {
    if crate::settings::snapshot().appearance.animations { seconds } else { 0.0 }
}

/// Rebuilds egui's style from the active palette and the appearance settings
/// (UI scale, animations). Cheap enough to call on every frame of a theme crossfade.
pub fn install(ctx: &egui::Context) {
    let p = palette();
    let settings = crate::settings::snapshot();
    let appearance = &settings.appearance;
    let mut style = (*ctx.global_style()).clone();
    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = bg();
    v.window_fill = surface_2();
    v.faint_bg_color = surface_2();
    v.window_stroke = Stroke::new(1.0, border());
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(RADIUS);
    v.selection.bg_fill = accent_soft();
    v.selection.stroke = Stroke::new(1.0, accent());
    v.hyperlink_color = blue();
    v.override_text_color = Some(text());
    v.window_shadow = egui::epaint::Shadow { offset: [0, 10], blur: 32, spread: 0, color: p.shadow };
    v.popup_shadow = egui::epaint::Shadow { offset: [0, 6], blur: 18, spread: 0, color: p.shadow.gamma_multiply(0.85) };
    v.text_cursor.stroke = Stroke::new(2.0, accent());
    v.extreme_bg_color = surface();
    v.code_bg_color = surface_3();
    v.warn_fg_color = warn();
    v.error_fg_color = danger();

    let w = &mut v.widgets;
    for (state, fill, stroke) in [
        (&mut w.noninteractive, surface_2(), border()),
        (&mut w.inactive, surface_2(), Color32::TRANSPARENT),
        (&mut w.hovered, surface_3(), border()),
        (&mut w.active, surface_3(), accent()),
        (&mut w.open, surface_3(), border()),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::new(1.0, stroke);
        state.corner_radius = CornerRadius::same(RADIUS_SM + 1);
    }
    w.noninteractive.fg_stroke = Stroke::new(1.0, text_dim());
    w.inactive.fg_stroke = Stroke::new(1.0, text());
    w.hovered.fg_stroke = Stroke::new(1.0, text());
    w.active.fg_stroke = Stroke::new(1.0, text());
    style.visuals = v;

    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.interact_size.y = 26.0;
    style.spacing.window_margin = Margin::same(16);
    style.animation_time = if appearance.animations { 0.16 } else { 0.0 };
    style.text_styles = [
        (TextStyle::Heading, FontId::new(18.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(13.0, FontFamily::Proportional)),
        (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
    ]
    .into();
    // Same style for both egui themes so the OS light/dark preference can't override ours.
    let style = std::sync::Arc::new(style);
    ctx.set_theme(if p.dark { egui::Theme::Dark } else { egui::Theme::Light });
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
    // UI scale lives in Settings; Ctrl +/- belongs to the terminal font.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    ctx.set_zoom_factor(appearance.ui_scale());
}

pub fn dock_style(egui_style: &egui::Style) -> DockStyle {
    let mut s = DockStyle::from_egui(egui_style);
    s.dock_area_padding = Some(Margin::same(6));
    s.main_surface_border_stroke = Stroke::NONE;
    s.separator.width = 6.0;
    s.separator.extra_interact_width = 4.0;
    s.separator.color_idle = bg();
    s.separator.color_hovered = border();
    s.separator.color_dragged = accent();

    s.tab_bar.bg_fill = bg();
    s.tab_bar.height = 34.0;
    s.tab_bar.hline_color = Color32::TRANSPARENT;
    s.tab_bar.corner_radius = CornerRadius::same(RADIUS);
    s.tab_bar.inner_margin = Margin::symmetric(4, 3);

    s.tab.spacing = 4.0;
    s.tab.minimum_width = Some(120.0);
    s.tab.hline_below_active_tab_name = false;
    let tab = |bg: Color32, text: Color32| egui_dock::TabInteractionStyle {
        outline_color: Color32::TRANSPARENT,
        corner_radius: CornerRadius::same(RADIUS_SM + 1),
        bg_fill: bg,
        text_color: text,
    };
    s.tab.active = tab(surface_2(), text());
    s.tab.focused = tab(surface_3(), text());
    s.tab.hovered = tab(surface_2(), text());
    s.tab.inactive = tab(Color32::TRANSPARENT, text_dim());
    s.tab.active_with_kb_focus = s.tab.active.clone();
    s.tab.focused_with_kb_focus = s.tab.focused.clone();
    s.tab.inactive_with_kb_focus = s.tab.inactive.clone();
    s.tab.tab_body.bg_fill = surface();
    s.tab.tab_body.stroke = Stroke::new(1.0, border().gamma_multiply(0.55));
    s.tab.tab_body.corner_radius = CornerRadius::same(RADIUS);
    s.tab.tab_body.inner_margin = Margin::same(0);

    s.buttons.close_tab_color = text_faint();
    s.buttons.close_tab_active_color = text();
    s.buttons.close_tab_bg_fill = surface_3();
    s.buttons.add_tab_color = text_dim();
    s.buttons.add_tab_active_color = accent();
    s.buttons.add_tab_bg_fill = surface_2();

    s.overlay.selection_color = accent().gamma_multiply(0.35);
    s.overlay.button_color = surface_3();
    s.overlay.button_border_stroke = Stroke::new(1.0, accent());
    s.overlay.hovered_leaf_highlight.color = accent().gamma_multiply(0.08);
    s.overlay.hovered_leaf_highlight.stroke = Stroke::new(1.0, accent().gamma_multiply(0.6));
    s.overlay.hovered_leaf_highlight.corner_radius = CornerRadius::same(RADIUS);
    s
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn indexed_in_matches_global_lookup() {
        let p = Palette::TETHER_NIGHT;
        for i in [0u8, 7, 15] {
            assert_eq!(indexed_in(&p, i), p.ansi[i as usize]);
        }
        assert_eq!(indexed_in(&p, 196), indexed(196));
        assert_eq!(indexed_in(&p, 244), indexed(244));
    }

    #[test]
    fn indexed_palette_ranges() {
        assert_eq!(indexed(1), ansi(1));
        assert_eq!(indexed(16), Color32::from_rgb(0, 0, 0));
        assert_eq!(indexed(231), Color32::from_rgb(255, 255, 255));
        assert_eq!(indexed(232), Color32::from_rgb(8, 8, 8));
    }
}
