//! Terminal font choice: lists the monospace fonts installed on the system (scanned
//! once, in the background) and loads the chosen one in front of egui's bundled
//! monospace font, which stays as the fallback for missing glyphs.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

use crate::settings::{self, FontChoice};

const FONT_KEY: &str = "tether-terminal";
/// Font files above this size are CJK/emoji collections, never terminal fonts.
const MAX_FONT_BYTES: u64 = 12 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemFont {
    pub index: u32,
    pub family: String,
    pub path: PathBuf,
}

enum Scan {
    Idle,
    Running,
    Done(Arc<Vec<SystemFont>>),
}

static SCAN: Mutex<Scan> = Mutex::new(Scan::Idle);
/// Path currently loaded into egui (`None` = bundled font).
static APPLIED: Mutex<Option<Option<(String, u32)>>> = Mutex::new(None);
/// How many times [`apply`] replaced the fonts, and the pass it did it in (`set_fonts`
/// takes effect from the next pass).
static GENERATION: Mutex<(u64, u64)> = Mutex::new((0, 0));

/// Changes whenever the fonts in use change; caches of laid-out text key on it.
pub fn version(ctx: &egui::Context) -> u64 {
    let (generation, pass) = *GENERATION.lock().unwrap_or_else(|e| e.into_inner());
    // Still the old fonts during the pass that asked for new ones.
    if generation > 0 && ctx.cumulative_pass_nr() <= pass { generation - 1 } else { generation }
}

/// The installed monospace fonts, or `None` while the background scan runs (it
/// starts on the first call and repaints `ctx` when done).
pub fn system_monospace_fonts(ctx: &egui::Context) -> Option<Arc<Vec<SystemFont>>> {
    let mut scan = SCAN.lock().unwrap_or_else(|e| e.into_inner());
    match &*scan {
        Scan::Done(list) => return Some(list.clone()),
        Scan::Running => return None,
        Scan::Idle => {}
    }
    *scan = Scan::Running;
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new().name("tether-font-scan".into()).spawn(move || {
        let list = Arc::new(scan_dirs(&font_dirs()));
        *SCAN.lock().unwrap_or_else(|e| e.into_inner()) = Scan::Done(list);
        ctx.request_repaint();
    });
    if spawned.is_err() {
        *scan = Scan::Done(Arc::new(Vec::new()));
    }
    None
}

fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(win) = std::env::var_os("WINDIR") {
        dirs.push(PathBuf::from(win).join("Fonts"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("Microsoft").join("Windows").join("Fonts"));
    }
    if cfg!(not(windows)) {
        dirs.extend(
            ["/usr/share/fonts", "/usr/local/share/fonts", "/Library/Fonts", "/System/Library/Fonts", "/System/Library/Fonts/Supplemental"].map(PathBuf::from),
        );
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            dirs.push(home.join(".local/share/fonts"));
            dirs.push(home.join("Library/Fonts"));
        }
    }
    dirs
}

/// Regular monospace faces in `dirs` (one per family, sorted).
pub fn scan_dirs(dirs: &[PathBuf]) -> Vec<SystemFont> {
    let mut out: Vec<SystemFont> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for path in entries.flatten().map(|e| e.path()) {
            let is_font = path.extension().and_then(|e| e.to_str()).is_some_and(|e| ["ttf", "otf", "ttc"].iter().any(|x| x.eq_ignore_ascii_case(e)));
            let small = std::fs::metadata(&path).is_ok_and(|m| m.len() <= MAX_FONT_BYTES);
            if !is_font || !small {
                continue;
            }
            let Ok(data) = std::fs::read(&path) else { continue };
            if let Some((family, index)) = monospace_face(&data)
                && !out.iter().any(|f| f.family.eq_ignore_ascii_case(&family))
            {
                out.push(SystemFont { index, family, path });
            }
        }
    }
    out.sort_by_key(|f| f.family.to_lowercase());
    out
}

/// Family name of a regular (not bold/italic) monospace face.
pub fn monospace_family(data: &[u8]) -> Option<String> {
    monospace_face(data).map(|(family, _)| family)
}

fn monospace_face(data: &[u8]) -> Option<(String, u32)> {
    (0..ttf_parser::fonts_in_collection(data).unwrap_or(1).min(256)).find_map(|index| {
        let face = ttf_parser::Face::parse(data, index).ok()?;
        if face.is_bold() || face.is_italic() {
            return None;
        }
        let advance = |c: char| face.glyph_index(c).and_then(|g| face.glyph_hor_advance(g));
        let mono = face.is_monospaced() || matches!((advance('i'), advance('M'), advance('.')), (Some(a), Some(b), Some(c)) if a == b && b == c);
        if !mono || face.glyph_index('a').is_none() {
            return None;
        }
        let names = face.names();
        let pick = |id: u16| {
            names.into_iter().filter(|n| n.name_id == id && n.is_unicode()).find_map(|n| n.to_string()).or_else(|| {
                // Menlo on macOS uses a Macintosh Roman name table, not UTF-16.
                names.into_iter().filter(|n| n.name_id == id && n.platform_id == ttf_parser::PlatformId::Macintosh && n.encoding_id == 0).find_map(|n| {
                    let (name, _, errors) = encoding_rs::MACINTOSH.decode(n.name);
                    (!errors).then(|| name.into_owned())
                })
            })
        };
        pick(ttf_parser::name_id::TYPOGRAPHIC_FAMILY)
            .or_else(|| pick(ttf_parser::name_id::FAMILY))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(|family| (family, index))
    })
}

/// Font definitions with `choice` (if readable) in front of the bundled monospace font.
fn definitions(choice: Option<&FontChoice>) -> FontDefinitions {
    let mut defs = FontDefinitions::default();
    let Some(choice) = choice else { return defs };
    match read_font(Path::new(&choice.path), choice.index) {
        Ok(bytes) => {
            let mut data = FontData::from_owned(bytes);
            data.index = choice.index;
            defs.font_data.insert(FONT_KEY.into(), Arc::new(data));
            defs.families.entry(FontFamily::Monospace).or_default().insert(0, FONT_KEY.into());
        }
        Err(e) => eprintln!("tether: terminal font {}: {e}", choice.path),
    }
    defs
}

fn read_font(path: &Path, index: u32) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    ttf_parser::Face::parse(&bytes, index).map_err(|e| format!("not a font ({e})"))?;
    Ok(bytes)
}

/// Loads the terminal font from settings (no-op when it has not changed).
pub fn apply(ctx: &egui::Context) {
    let choice = settings::get().appearance.terminal_font;
    let key = choice.as_ref().map(|c| (c.path.clone(), c.index));
    {
        let mut applied = APPLIED.lock().unwrap_or_else(|e| e.into_inner());
        if applied.as_ref() == Some(&key) {
            return;
        }
        // Never set the bundled fonts again on first run: egui already has them.
        let first_default = applied.is_none() && key.is_none();
        *applied = Some(key);
        if first_default {
            return;
        }
    }
    ctx.set_fonts(definitions(choice.as_ref()));
    let mut generation = GENERATION.lock().unwrap_or_else(|e| e.into_inner());
    *generation = (generation.0 + 1, ctx.cumulative_pass_nr());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_system_font_collections_are_discovered_and_loadable() {
        let fonts = scan_dirs(&font_dirs());
        let menlo = fonts.iter().find(|f| f.family == "Menlo").expect("macOS ships Menlo.ttc");
        assert!(read_font(&menlo.path, menlo.index).is_ok());
        let defs = definitions(Some(&FontChoice { index: menlo.index, family: menlo.family.clone(), path: menlo.path.display().to_string() }));
        assert_eq!(defs.families[&FontFamily::Monospace][0], FONT_KEY);
    }

    #[test]
    fn non_fonts_are_rejected() {
        assert_eq!(monospace_family(b"definitely not a font"), None);
        assert!(read_font(Path::new("does/not/exist.ttf"), 0).is_err());
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("fake.ttf"), b"nope").unwrap();
        assert!(scan_dirs(&[dir.path().to_path_buf()]).is_empty());
    }

    #[test]
    fn unreadable_choice_keeps_the_bundled_fonts() {
        let defs = definitions(Some(&FontChoice { index: 0, family: "Ghost".into(), path: "missing.ttf".into() }));
        assert!(!defs.font_data.contains_key(FONT_KEY));
        assert_eq!(defs.families[&FontFamily::Monospace], FontDefinitions::default().families[&FontFamily::Monospace]);
    }

    /// Windows ships Consolas; when present it must be found and loadable.
    #[test]
    fn finds_consolas_on_windows() {
        let Some(win) = std::env::var_os("WINDIR") else { return };
        let path = PathBuf::from(win).join("Fonts").join("consola.ttf");
        let Ok(data) = std::fs::read(&path) else { return };
        assert_eq!(monospace_family(&data).as_deref(), Some("Consolas"));
        let defs = definitions(Some(&FontChoice { index: 0, family: "Consolas".into(), path: path.display().to_string() }));
        assert_eq!(defs.families[&FontFamily::Monospace][0], FONT_KEY);
    }
}
