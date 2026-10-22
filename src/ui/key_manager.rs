//! Key manager (a simpler Bitvise client key manager): list, generate, import, view
//! (fingerprint, randomart, public key), rename / re-comment, change the passphrase,
//! export, install on a host and delete. Everything that runs the key KDF or generates
//! a key (tens of ms to seconds) happens on a worker thread; the UI shows a spinner.

mod details;
mod generate;

use std::sync::{Arc, Mutex};

use eframe::egui::{self, Align2, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};
use russh::keys::PrivateKey;
use uuid::Uuid;

use super::form::{self, Kind};
use super::settings_widgets::{self as w, Icon};
use crate::keystore::{KeyKind, KeyMeta, KeyStore, Rewrite};
use crate::profile::{AuthMethod, Profile, ProfileStore, secret};
use crate::theme;

const LIST_W: f32 = 300.0;
const ROW_H: f32 = 64.0;
const PAD: f32 = 20.0;

pub enum KeyOutcome {
    Open,
    Close,
    /// A key was generated or imported while the host editor asked for one.
    Created(Uuid),
    /// Append the public key to `~/.ssh/authorized_keys` on this host.
    Install {
        key: Uuid,
        host: Uuid,
    },
}

#[derive(Clone)]
struct GenForm {
    name: String,
    kind: KeyKind,
    comment: String,
    pass: String,
    pass2: String,
    show: bool,
    remember: bool,
}

impl Default for GenForm {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: KeyKind::Ed25519,
            comment: crate::keystore::default_comment(),
            pass: String::new(),
            pass2: String::new(),
            show: false,
            remember: true,
        }
    }
}

enum Panel {
    Details,
    Generate(GenForm),
}

/// Inline form under the detail actions.
enum Form {
    None,
    Edit { name: String, comment: String, pass: String },
    Passphrase { old: String, new: String, new2: String, remember: bool },
    Delete,
}

/// What a finished worker job hands back to the UI thread.
enum Done {
    /// A new key, already encrypted as the form asked.
    Generated { form: GenForm, key: Box<PrivateKey> },
    /// A re-encoded key file, plus what to do with the label and the keyring.
    Rewritten { rewrite: Rewrite, id: Uuid, name: Option<String>, remember: Remember, note: String },
}

/// Keyring change applied once a passphrase change succeeded.
enum Remember {
    Keep,
    Store(String),
    Forget,
}

type Job = Arc<Mutex<Option<anyhow::Result<Done>>>>;

pub struct KeyManager {
    selected: Option<Uuid>,
    panel: Panel,
    form: Form,
    /// Worker job in flight (generation or a passphrase / comment rewrite).
    pending: Option<Job>,
    message: Option<(bool, String)>,
    /// Hand the next created key back to the host editor.
    for_editor: bool,
    install_host: Option<Uuid>,
    /// Show / hide state of the passphrase fields in the inline forms.
    show_pass: bool,
    opened_at: Option<f64>,
    key_search: Option<super::key_search::KeySearch>,
}

impl KeyManager {
    pub fn new(keys: &KeyStore) -> Self {
        Self {
            selected: keys.keys.first().map(|k| k.id),
            panel: Panel::Details,
            form: Form::None,
            pending: None,
            message: None,
            for_editor: false,
            install_host: None,
            show_pass: false,
            opened_at: None,
            key_search: None,
        }
    }

    /// Opens straight on the generate form; the new key is reported as `Created`.
    pub fn generate_for_editor(keys: &KeyStore) -> Self {
        Self { panel: Panel::Generate(GenForm::default()), for_editor: true, ..Self::new(keys) }
    }

    /// Opened from the host editor's "Manage": imports are handed back to it too.
    pub fn for_editor(mut self) -> Self {
        self.for_editor = true;
        self
    }

    pub fn show(&mut self, ctx: &egui::Context, keys: &mut KeyStore, profiles: &ProfileStore) -> KeyOutcome {
        let mut outcome = self.poll_job(keys);
        if self.pending.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
        let now = ctx.input(|i| i.time);
        let opened = *self.opened_at.get_or_insert(now);
        let t = (((now - opened) / theme::anim_time(0.2).max(1e-3) as f64) as f32).clamp(0.0, 1.0);
        if t < 1.0 {
            ctx.request_repaint();
        }
        let screen = ctx.content_rect();
        let size = Vec2::new((screen.width() - 64.0).clamp(640.0, 960.0), (screen.height() - 72.0).clamp(420.0, 640.0));
        let modal = egui::Modal::new(egui::Id::new("key_manager")).frame(egui::Frame::NONE).show(ctx, |ui| {
            ui.multiply_opacity(1.0 - (1.0 - t).powi(3));
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            let p = ui.painter();
            p.add(egui::Shadow { offset: [0, 18], blur: 48, spread: 0, color: theme::shadow() }.as_shape(rect, CornerRadius::same(16)));
            p.rect(rect, CornerRadius::same(16), theme::surface(), Stroke::new(1.0, theme::border()), egui::StrokeKind::Inside);
            let list = Rect::from_min_max(rect.min, Pos2::new(rect.left() + LIST_W, rect.bottom()));
            p.rect_filled(list.shrink(1.0), CornerRadius { nw: 15, sw: 15, ne: 0, se: 0 }, theme::bg());
            p.vline(list.right(), list.y_range(), Stroke::new(1.0, theme::border()));
            let close_rect = Rect::from_center_size(Pos2::new(rect.right() - 30.0, rect.top() + 30.0), Vec2::splat(30.0));
            if ui.scope_builder(egui::UiBuilder::new().max_rect(close_rect), |ui| w::close_button(ui, 30.0, false)).inner.on_hover_text("Close (Esc)").clicked()
            {
                outcome = KeyOutcome::Close;
            }
            ui.scope_builder(egui::UiBuilder::new().max_rect(list.shrink2(Vec2::new(16.0, PAD))), |ui| self.list(ui, keys, profiles, &mut outcome));
            let body = Rect::from_min_max(Pos2::new(list.right() + 28.0, rect.top() + PAD + 2.0), rect.max - Vec2::new(14.0, 14.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    egui::Frame::new().inner_margin(egui::Margin { left: 0, right: 42, top: 0, bottom: 12 }).show(ui, |ui| {
                        if matches!(self.panel, Panel::Generate(_)) {
                            self.generate_panel(ui);
                        } else {
                            self.details(ui, keys, profiles, &mut outcome);
                        }
                    });
                });
            });
        });
        if let Some(search) = &mut self.key_search {
            let (path, close) = search.show(ctx);
            if close {
                self.key_search = None;
            }
            if let Some(path) = path {
                self.import_path(&path, keys, &mut outcome);
            }
        }
        if modal.should_close() && self.key_search.is_none() && matches!(outcome, KeyOutcome::Open) && self.pending.is_none() {
            outcome = KeyOutcome::Close;
        }
        outcome
    }

    fn list(&mut self, ui: &mut Ui, keys: &mut KeyStore, profiles: &ProfileStore, outcome: &mut KeyOutcome) {
        ui.horizontal(|ui| {
            form::icon_tile(ui, Icon::Key, theme::accent());
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new("SSH keys").size(17.0).strong());
                ui.label(RichText::new("Create once, use on every host").size(12.0).color(theme::text_dim()));
            });
        });
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if form::button(ui, "+ Generate", Kind::Primary, self.pending.is_none()).clicked() {
                self.start_generate_form();
            }
            if form::button(ui, "Import…", Kind::Secondary, true).on_hover_text("Add an existing OpenSSH private key file").clicked() {
                self.import(keys, outcome);
            }
        });
        ui.add_space(16.0);
        if !keys.keys.is_empty() {
            ui.label(
                RichText::new(format!("{} KEY{}", keys.keys.len(), if keys.keys.len() == 1 { "" } else { "S" })).size(10.5).strong().color(theme::text_faint()),
            );
            ui.add_space(6.0);
        }
        if ui.button("Search this PC for SSH keys…").clicked() {
            self.key_search = Some(super::key_search::KeySearch::new(ui.ctx()));
        }
        egui::ScrollArea::vertical().id_salt("key_list").auto_shrink([false, false]).show(ui, |ui| {
            if keys.keys.is_empty() {
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("No keys yet").size(13.5).color(theme::text_dim()));
                    ui.label(RichText::new("Generate one, or import ~/.ssh/id_*").size(12.0).color(theme::text_faint()));
                });
            }
            for k in &keys.keys {
                let uses = profiles.profiles.iter().filter(|p| uses_key(p, k.id)).count();
                let selected = self.selected == Some(k.id) && matches!(self.panel, Panel::Details);
                if key_row(ui, k, uses, selected).clicked() {
                    self.select(k.id);
                }
                ui.add_space(4.0);
            }
        });
    }

    fn start_generate_form(&mut self) {
        self.panel = Panel::Generate(GenForm::default());
        self.form = Form::None;
        self.message = None;
    }

    fn import(&mut self, keys: &mut KeyStore, outcome: &mut KeyOutcome) {
        let Some(path) = rfd::FileDialog::new().set_title("Import private key").pick_file() else { return };
        self.import_path(&path, keys, outcome);
    }

    fn import_path(&mut self, path: &std::path::Path, keys: &mut KeyStore, outcome: &mut KeyOutcome) {
        match keys.import_file(path) {
            Ok(meta) => {
                self.select(meta.id);
                self.message = Some((true, format!("Imported “{}”", meta.name)));
                if self.for_editor {
                    *outcome = KeyOutcome::Created(meta.id);
                }
            }
            Err(e) => self.message = Some((false, format!("{e:#}"))),
        }
    }

    fn select(&mut self, id: Uuid) {
        self.selected = Some(id);
        self.panel = Panel::Details;
        self.form = Form::None;
        self.install_host = None;
    }

    /// Applies a finished worker job to the store (fast file writes only).
    fn poll_job(&mut self, keys: &mut KeyStore) -> KeyOutcome {
        let Some(job) = &self.pending else { return KeyOutcome::Open };
        let Some(result) = job.lock().ok().and_then(|mut s| s.take()) else { return KeyOutcome::Open };
        self.pending = None;
        match result {
            Ok(Done::Generated { form, key }) => {
                let name = if form.name.trim().is_empty() { format!("{} key", form.kind.label()) } else { form.name.clone() };
                match keys.add_key(&name, *key) {
                    Ok(meta) => {
                        let mut note = format!("Generated “{}”", meta.name);
                        if form.remember
                            && !form.pass.is_empty()
                            && let Err(e) = secret::set(meta.id, &form.pass)
                        {
                            note = format!("{note} (couldn't remember the passphrase: {e:#})");
                        }
                        self.select(meta.id);
                        self.message = Some((true, note));
                        if self.for_editor {
                            return KeyOutcome::Created(meta.id);
                        }
                    }
                    Err(e) => self.message = Some((false, format!("{e:#}"))),
                }
            }
            Ok(Done::Rewritten { rewrite, id, name, remember, note }) => {
                let applied = keys.apply(rewrite).and_then(|()| {
                    if let Some(n) = &name {
                        keys.rename(id, n);
                        keys.save()?;
                    }
                    Ok(())
                });
                self.message = Some(match applied {
                    Err(e) => (false, format!("{e:#}")),
                    Ok(()) => {
                        let stored = match remember {
                            Remember::Keep => Ok(()),
                            Remember::Store(p) => secret::set(id, &p),
                            Remember::Forget => secret::delete(id),
                        };
                        match stored {
                            Ok(()) => (true, note),
                            Err(e) => (false, format!("{note}, but the keyring failed: {e:#}")),
                        }
                    }
                });
            }
            Err(e) => self.message = Some((false, format!("{e:#}"))),
        }
        KeyOutcome::Open
    }

    fn message_line(&self, ui: &mut Ui) {
        if let Some((ok, text)) = &self.message {
            ui.add_space(12.0);
            form::banner(ui, *ok, text);
        }
    }

    /// Shows the result of an install started from here.
    pub fn report(&mut self, ok: bool, text: String) {
        self.message = Some((ok, text));
    }
}

/// Runs `work` on a worker thread; the UI polls the returned slot.
fn spawn(work: impl FnOnce() -> anyhow::Result<Done> + Send + 'static) -> Job {
    let slot: Job = Arc::default();
    let out = slot.clone();
    std::thread::spawn(move || {
        let result = work();
        if let Ok(mut s) = out.lock() {
            *s = Some(result);
        }
    });
    slot
}

fn uses_key(p: &Profile, key: Uuid) -> bool {
    matches!(p.auth, AuthMethod::Managed { key: k } if k == key)
}

fn key_row(ui: &mut Ui, k: &KeyMeta, uses: usize, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &k.name));
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let ctx = ui.ctx();
    let h = ctx.animate_bool_with_time(resp.id, resp.hovered(), theme::anim_time(0.12));
    let s = ctx.animate_bool_with_time(resp.id.with("sel"), selected, theme::anim_time(0.18));
    let p = ui.painter();
    let fill = theme::surface_2().gamma_multiply((0.6 * h).max(s));
    p.rect(rect, CornerRadius::same(10), fill, Stroke::new(1.0, theme::border().gamma_multiply(s)), egui::StrokeKind::Inside);
    if s > 0.0 {
        let bar = Rect::from_center_size(Pos2::new(rect.left() + 1.5, rect.center().y), Vec2::new(3.0, 26.0 * s));
        p.rect_filled(bar, CornerRadius::same(2), theme::accent());
    }
    let icon = Rect::from_center_size(Pos2::new(rect.left() + 28.0, rect.center().y), Vec2::splat(36.0));
    p.rect_filled(icon, CornerRadius::same(9), theme::accent().gamma_multiply(0.15));
    w::paint_icon(p, icon.shrink(10.0), Icon::Key, theme::accent());
    if k.encrypted {
        let c = Pos2::new(icon.right() - 3.0, icon.bottom() - 3.0);
        p.circle_filled(c, 7.0, theme::bg());
        lock_glyph(p, c, theme::warn());
    }
    let x = icon.right() + 12.0;
    let clip = p.with_clip_rect(Rect::from_min_max(Pos2::new(x, rect.top()), rect.max - Vec2::new(10.0, 0.0)));
    clip.text(Pos2::new(x, rect.top() + 22.0), Align2::LEFT_CENTER, &k.name, FontId::proportional(13.5), theme::text());
    let short: String = k.fingerprint.trim_start_matches("SHA256:").chars().take(10).collect();
    let hosts = if uses > 0 { format!("  ·  {uses} host{}", if uses == 1 { "" } else { "s" }) } else { String::new() };
    clip.text(
        Pos2::new(x, rect.top() + 42.0),
        Align2::LEFT_CENTER,
        format!("{}  ·  {short}…{hosts}", k.algorithm),
        FontId::proportional(11.5),
        theme::text_faint(),
    );
    resp
}

fn lock_glyph(p: &egui::Painter, c: Pos2, col: egui::Color32) {
    let body = Rect::from_center_size(c + Vec2::new(0.0, 1.2), Vec2::new(7.0, 5.0));
    p.rect_filled(body, CornerRadius::same(1), col);
    p.circle_stroke(c + Vec2::new(0.0, -1.6), 2.2, Stroke::new(1.2, col));
}

/// A file name derived from the key name.
fn file_stem(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    if s.trim_matches('_').is_empty() { "id_tether".into() } else { format!("id_{}", s.to_lowercase()) }
}

/// `2026-10-17` from unix seconds (UTC).
fn date(unix: u64) -> String {
    if unix == 0 {
        return "unknown".into();
    }
    // Howard Hinnant's civil-from-days.
    let z = (unix / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keystore;

    #[test]
    fn dates_and_file_names() {
        assert_eq!(date(0), "unknown");
        assert_eq!(date(1_760_659_200), "2025-10-17");
        assert_eq!(date(951_782_400), "2000-02-29");
        assert_eq!(file_stem("My Laptop!"), "id_my_laptop_");
        assert_eq!(file_stem("***"), "id_tether");
    }

    #[test]
    fn renders_headless_with_and_without_keys() {
        let dir = tempfile::tempdir().unwrap();
        let mut keys = KeyStore::load(dir.path().join("keys")).unwrap();
        let profiles = ProfileStore::default();
        let ctx = egui::Context::default();
        let frame = |m: &mut KeyManager, keys: &mut KeyStore| {
            let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                m.show(ui.ctx(), keys, &profiles);
            });
            out.textures_delta.clear();
        };
        let mut m = KeyManager::new(&keys);
        frame(&mut m, &mut keys);
        keys.add_generated("k", keystore::generate(KeyKind::Ed25519, "a@b").unwrap(), None).unwrap();
        let mut m = KeyManager::new(&keys);
        frame(&mut m, &mut keys);
        let mut g = KeyManager::generate_for_editor(&keys);
        frame(&mut g, &mut keys);
    }
}
