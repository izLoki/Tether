//! User settings persisted to `<config>/settings.json`. Every field has a serde
//! default so older files keep loading as new options are added.
//!
//! Access goes through a global (`settings::get()` / `settings::update(..)`) so
//! widgets deep in the tree read the current values without plumbing.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::profile::write_json_atomic;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CursorStyle {
    #[default]
    Block,
    Bar,
    Underline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum OpenWith {
    /// The built-in editor tab (saves straight back to the server).
    #[default]
    Editor,
    /// The Windows default app; edits are watched and uploaded on save (Bitvise-style).
    System,
}

/// An image painted behind the terminal text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct BackgroundImage {
    pub path: String,
    /// 0..=1, how strongly the image shows through.
    pub opacity: f32,
}

impl BackgroundImage {
    pub fn opacity(&self) -> f32 {
        sane(self.opacity, 0.0, 1.0, 0.3)
    }
}

/// A terminal font file picked from the system fonts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FontChoice {
    pub family: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// Name of a built-in or user theme.
    pub theme: String,
    pub ui_scale: f32,
    pub animations: bool,
    /// Follow the OS light/dark mode with `light_theme` / `dark_theme`.
    pub sync_with_os: bool,
    pub light_theme: String,
    pub dark_theme: String,
    /// 0.5..=1; below 1 the window background lets the desktop show through.
    pub window_opacity: f32,
    /// Lower-case theme name -> image painted behind its terminals.
    pub background_images: BTreeMap<String, BackgroundImage>,
    /// `None` keeps the bundled monospace font.
    pub terminal_font: Option<FontChoice>,
}

impl Appearance {
    pub const SCALE_MIN: f32 = 0.75;
    pub const SCALE_MAX: f32 = 1.75;

    pub const OPACITY_MIN: f32 = 0.5;

    pub fn ui_scale(&self) -> f32 {
        sane(self.ui_scale, Self::SCALE_MIN, Self::SCALE_MAX, 1.0)
    }

    pub fn window_opacity(&self) -> f32 {
        sane(self.window_opacity, Self::OPACITY_MIN, 1.0, 1.0)
    }

    /// The user-chosen background image of `theme`, if any.
    pub fn background_image(&self, theme: &str) -> Option<&BackgroundImage> {
        self.background_images.get(&theme.trim().to_lowercase()).filter(|b| !b.path.trim().is_empty())
    }
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: "Tether Night".into(),
            ui_scale: 1.0,
            animations: true,
            sync_with_os: false,
            light_theme: "Tether Day".into(),
            dark_theme: "Tether Night".into(),
            window_opacity: 1.0,
            background_images: BTreeMap::new(),
            terminal_font: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalSettings {
    pub font_size: f32,
    pub line_height: f32,
    pub padding: f32,
    pub cursor_style: CursorStyle,
    pub cursor_blink: bool,
    pub scrollback: usize,
    pub copy_on_select: bool,
    pub visual_bell: bool,
    /// Thin overlay scrollbar that fades in while scrolling through history.
    pub scrollbar: bool,
    /// Shell id (`pty::shells`) opened by "New terminal"; empty = the first one found.
    pub default_shell: String,
}

impl TerminalSettings {
    pub const FONT_MIN: f32 = 8.0;
    pub const FONT_MAX: f32 = 32.0;
    pub const LINE_HEIGHT_MIN: f32 = 0.9;
    pub const LINE_HEIGHT_MAX: f32 = 1.8;
    pub const PADDING_MAX: f32 = 32.0;
    pub const SCROLLBACK_MIN: usize = 500;
    pub const SCROLLBACK_MAX: usize = 100_000;

    pub fn font_size(&self) -> f32 {
        sane(self.font_size, Self::FONT_MIN, Self::FONT_MAX, 13.5)
    }

    pub fn line_height(&self) -> f32 {
        sane(self.line_height, Self::LINE_HEIGHT_MIN, Self::LINE_HEIGHT_MAX, 1.0)
    }

    pub fn padding(&self) -> f32 {
        sane(self.padding, 0.0, Self::PADDING_MAX, 10.0)
    }

    pub fn scrollback(&self) -> usize {
        self.scrollback.clamp(Self::SCROLLBACK_MIN, Self::SCROLLBACK_MAX)
    }
}

/// Clamps a value read from disk; NaN/inf (hand-edited files) fall back to `default`.
fn sane(v: f32, min: f32, max: f32, default: f32) -> f32 {
    if v.is_finite() { v.clamp(min, max) } else { default }
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_size: 13.5,
            line_height: 1.0,
            padding: 10.0,
            cursor_style: CursorStyle::Block,
            cursor_blink: true,
            scrollback: 10_000,
            copy_on_select: false,
            visual_bell: true,
            scrollbar: true,
            default_shell: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SftpSettings {
    pub show_hidden: bool,
    pub start_with_local_pane: bool,
    pub confirm_delete: bool,
    /// Reopen each host in the folder its last SFTP session ended in.
    pub resume_last_dir: bool,
}

impl Default for SftpSettings {
    fn default() -> Self {
        Self { show_hidden: true, start_with_local_pane: false, confirm_delete: true, resume_last_dir: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    /// Ask GitHub for a newer release when tether starts (release builds only).
    pub check_on_startup: bool,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self { check_on_startup: true }
    }
}

/// AI agent sessions (`agents`) and the per-terminal resource monitor (`procmon`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSettings {
    /// CPU / memory chip on local terminal tabs and the app total in the top bar.
    pub show_usage: bool,
    /// Status badges on tabs of terminals running an agent.
    pub badges: bool,
    /// Native notification when a session needs you while tether is in the background.
    pub desktop_notifications: bool,
    /// Flash the taskbar button in the same case.
    pub flash_taskbar: bool,
    /// Also notify when a session finishes a turn (not only when it asks something).
    pub notify_on_done: bool,
    /// Sampling period of the resource monitor (slower while tether is in the background).
    pub monitor_interval_ms: u64,
    /// Shell chosen in Custom Boot, by agent id. Separate from the normal terminal default.
    pub launch_shells: BTreeMap<String, String>,
}

impl AgentSettings {
    pub const INTERVAL_MIN_MS: u64 = 500;
    pub const INTERVAL_MAX_MS: u64 = 10_000;

    pub fn monitor_interval(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.monitor_interval_ms.clamp(Self::INTERVAL_MIN_MS, Self::INTERVAL_MAX_MS))
    }
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            show_usage: true,
            badges: true,
            desktop_notifications: true,
            flash_taskbar: true,
            notify_on_done: true,
            monitor_interval_ms: 1500,
            launch_shells: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    pub font_size: f32,
    pub tab_size: usize,
    pub word_wrap: bool,
    pub line_numbers: bool,
}

impl EditorSettings {
    pub const FONT_MIN: f32 = 9.0;
    pub const FONT_MAX: f32 = 28.0;
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self { font_size: 13.0, tab_size: 4, word_wrap: false, line_numbers: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    pub projects: crate::local_projects::Projects,
    pub appearance: Appearance,
    pub terminal: TerminalSettings,
    pub sftp: SftpSettings,
    pub editor: EditorSettings,
    pub updates: UpdateSettings,
    pub agents: AgentSettings,
    /// Command blocks, cwd and git branch reported by local shells (`shell_integration`).
    pub shell_integration: crate::shell_integration::ShellIntegrationSettings,
    /// Lower-case extension (no dot) -> how double-click opens it. Missing = `default_open`.
    pub open_with: BTreeMap<String, OpenWith>,
    pub default_open: OpenWith,
    /// User overrides of the default shortcuts (`None` = unbound).
    #[serde(deserialize_with = "crate::keybindings::deserialize_overrides")]
    pub keybindings: BTreeMap<crate::keybindings::Action, Option<crate::keybindings::Binding>>,
    /// Where tabs, the sidebar and its sections go; pane behaviour.
    pub layout: crate::layout::LayoutSettings,
}

impl Settings {
    pub fn open_with_for(&self, file_name: &str) -> OpenWith {
        let ext = file_name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        self.open_with.get(&ext).copied().unwrap_or(self.default_open)
    }
}

/// Shared, copy-on-write: readers take a cheap `Arc` clone (see [`snapshot`]).
static CURRENT: RwLock<Option<Arc<Settings>>> = RwLock::new(None);
static PATH: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Loads settings from `path` (defaults if missing; a corrupt file is kept as `.bak`) and makes them current.
pub fn init(path: PathBuf) -> Settings {
    let loaded: Settings = crate::json_file::load_or_default(&path);
    *PATH.write().unwrap_or_else(|e| e.into_inner()) = Some(path);
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(loaded.clone()));
    loaded
}

/// An owned copy of the current settings. Clones every map: prefer [`snapshot`] on hot paths.
pub fn get() -> Settings {
    (*snapshot()).clone()
}

/// The current settings without copying them (an `Arc` clone). Cheap enough to call per
/// frame or per event; a later `update`/`preview` swaps in a new value and leaves this one intact.
pub fn snapshot() -> Arc<Settings> {
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(default_snapshot)
}

fn default_snapshot() -> Arc<Settings> {
    static DEFAULT: std::sync::OnceLock<Arc<Settings>> = std::sync::OnceLock::new();
    DEFAULT.get_or_init(|| Arc::new(Settings::default())).clone()
}

/// Applies `f` to the current settings and persists them.
pub fn update(f: impl FnOnce(&mut Settings)) -> anyhow::Result<()> {
    preview(f);
    save()
}

/// Applies `f` in memory only. Use while a slider is being dragged, then call `save`.
pub fn preview(f: impl FnOnce(&mut Settings)) {
    let mut guard = CURRENT.write().unwrap_or_else(|e| e.into_inner());
    // Copy-on-write: snapshots handed out earlier keep the old value.
    f(Arc::make_mut(guard.get_or_insert_with(|| Arc::new(Settings::default()))));
}

/// Writes the current settings to disk (no-op before `init`).
pub fn save() -> anyhow::Result<()> {
    let current = snapshot();
    match PATH.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some(path) => write_json_atomic(path, &*current),
        None => Ok(()),
    }
}

/// Where settings are stored, once `init` has run.
pub fn path() -> Option<PathBuf> {
    PATH.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Normalises a user-typed extension (`.TXT` -> `txt`); `None` if it isn't a plain extension.
pub fn normalize_extension(raw: &str) -> Option<String> {
    let ext = raw.trim().trim_start_matches('.').to_ascii_lowercase();
    let valid = !ext.is_empty() && ext.len() <= 16 && ext.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) && !ext.ends_with('.');
    valid.then_some(ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_with_falls_back_to_default() {
        let mut s = Settings::default();
        s.open_with.insert("png".into(), OpenWith::System);
        assert_eq!(s.open_with_for("logo.PNG"), OpenWith::System);
        assert_eq!(s.open_with_for("app.conf"), OpenWith::Editor);
        assert_eq!(s.open_with_for("Makefile"), OpenWith::Editor);
    }

    #[test]
    fn partial_json_fills_defaults() {
        let s: Settings = serde_json::from_str(r#"{"terminal":{"font_size":16.0}}"#).unwrap();
        assert_eq!(s.terminal.font_size, 16.0);
        assert_eq!(s.terminal.scrollback, 10_000);
        assert_eq!(s.appearance.theme, "Tether Night");
        assert!(s.terminal.scrollbar);
        assert!(s.sftp.resume_last_dir);
        assert!(s.keybindings.is_empty());
        assert_eq!(s.appearance.window_opacity(), 1.0);
        assert!(!s.appearance.sync_with_os && s.appearance.terminal_font.is_none());
    }

    #[test]
    fn appearance_extras_round_trip_and_clamp() {
        let mut a = Appearance { window_opacity: 0.1, sync_with_os: true, ..Default::default() };
        assert_eq!(a.window_opacity(), Appearance::OPACITY_MIN);
        a.background_images.insert("nord".into(), BackgroundImage { path: "C:/bg.png".into(), opacity: 7.0 });
        a.terminal_font = Some(FontChoice { family: "Cascadia Mono".into(), path: "C:/Windows/Fonts/CascadiaMono.ttf".into() });
        assert_eq!(a.background_image(" Nord ").map(|b| b.opacity()), Some(1.0));
        assert!(a.background_image("Dracula").is_none());
        let back: Appearance = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn round_trips_through_json() {
        let mut s = Settings::default();
        s.appearance.theme = "Nord".into();
        s.appearance.ui_scale = 1.25;
        s.terminal.cursor_style = CursorStyle::Underline;
        s.terminal.line_height = 1.3;
        s.terminal.scrollbar = false;
        s.editor.tab_size = 2;
        s.open_with.insert("log".into(), OpenWith::System);
        s.default_open = OpenWith::System;
        s.keybindings.insert(crate::keybindings::Action::CloseTab, None);
        s.keybindings.insert(crate::keybindings::Action::NextTab, Some(crate::keybindings::Binding::new(eframe::egui::Key::PageDown, true, false, false)));
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn accessors_clamp_bad_values() {
        let mut t = TerminalSettings { font_size: 400.0, line_height: f32::NAN, padding: -3.0, scrollback: 5, ..Default::default() };
        assert_eq!(t.font_size(), TerminalSettings::FONT_MAX);
        assert_eq!(t.line_height(), 1.0);
        assert_eq!(t.padding(), 0.0);
        assert_eq!(t.scrollback(), TerminalSettings::SCROLLBACK_MIN);
        t.scrollback = usize::MAX;
        assert_eq!(t.scrollback(), TerminalSettings::SCROLLBACK_MAX);
        let a = Appearance { ui_scale: 9.0, ..Default::default() };
        assert_eq!(a.ui_scale(), Appearance::SCALE_MAX);
    }

    #[test]
    fn snapshots_are_copy_on_write() {
        let before = snapshot();
        assert!(Arc::ptr_eq(&before, &snapshot()), "reads share one allocation");
        // A write while a snapshot is held copies instead of mutating the held value.
        preview(|_| {});
        let after = snapshot();
        assert!(!Arc::ptr_eq(&before, &after));
        assert_eq!(*before, *after);
    }

    #[test]
    fn extensions_are_normalised() {
        assert_eq!(normalize_extension(" .TXT ").as_deref(), Some("txt"));
        assert_eq!(normalize_extension("tar.gz").as_deref(), Some("tar.gz"));
        assert_eq!(normalize_extension(""), None);
        assert_eq!(normalize_extension("a b"), None);
        assert_eq!(normalize_extension("../x"), None);
        assert_eq!(normalize_extension("x".repeat(17).as_str()), None);
    }
}
