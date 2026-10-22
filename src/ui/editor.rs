//! Host editor modal: a header that previews the host, then General / Connection /
//! Authentication section cards with stacked labels, tall inputs and inline validation,
//! a collapsible Advanced card and a sticky footer with "Test connection" (a real
//! handshake in the background), Cancel and Save. Enter saves, Esc closes.

mod auth;
#[cfg(test)]
mod shots;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Align, Align2, CornerRadius, FontId, Margin, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};
use egui::collapsing_header::CollapsingState;
use uuid::Uuid;

use super::autocomplete::AutoField;
use super::form::{self, Kind};
use super::loading;
use super::settings_widgets::{self as w, Icon};
use super::suggest;
use crate::folders::FolderStore;
use crate::keystore::KeyStore;
use crate::profile::{AuthMethod, MAX_PIPELINE_KB, MIN_PIPELINE_KB, Profile, secret};
use crate::ssh::connection::Stage;
use crate::theme;

const MAX_WIDTH: f32 = 680.0;
const PAD: i8 = 24;
const PORT_W: f32 = 112.0;
/// Accent presets offered next to the hue slider.
const SWATCHES: [f32; 8] = [150.0, 200.0, 220.0, 265.0, 300.0, 340.0, 20.0, 45.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthTab {
    Password,
    Managed,
    File,
}

/// Progress of a "Test connection" run, written by the background task.
#[derive(Debug, Clone, PartialEq)]
pub enum TestState {
    Idle,
    Running(Stage),
    Ok(u128),
    Failed(String),
}

pub type TestSlot = Arc<Mutex<TestState>>;

pub struct ProfileEditor {
    pub profile: Profile,
    secret: String,
    show_secret: bool,
    has_saved_secret: bool,
    is_new: bool,
    /// Validation runs on save; afterwards errors update live.
    tried_save: bool,
    auth_tab: AuthTab,
    /// Name typed for a folder to create on save.
    new_folder: Option<String>,
    /// The port as typed (validated into `profile.port`).
    port_text: String,
    users: Vec<String>,
    hosts: Vec<String>,
    user_ac: AutoField,
    host_ac: AutoField,
    test: TestSlot,
    key_search: Option<super::key_search::KeySearch>,
}

pub enum EditorOutcome {
    Open,
    Cancel,
    Save(Box<SaveRequest>),
    /// Open the key manager on "generate"; the new key is then selected here.
    GenerateKey,
    ManageKeys,
    /// Try to connect with these settings (result goes into the editor's test slot).
    Test(Box<Profile>, Option<String>, TestSlot),
}

pub struct SaveRequest {
    pub profile: Profile,
    /// New secret (None = keep whatever is stored).
    pub secret: Option<String>,
    /// Create this top-level folder and put the host in it.
    pub new_folder: Option<String>,
}

impl ProfileEditor {
    pub fn new(profile: Profile, is_new: bool) -> Self {
        let has_saved_secret = !is_new && secret::get(profile.secret_id()).is_some();
        let auth_tab = match profile.auth {
            AuthMethod::Password => AuthTab::Password,
            AuthMethod::Managed { .. } => AuthTab::Managed,
            AuthMethod::Key { .. } => AuthTab::File,
        };
        Self {
            port_text: profile.port.to_string(),
            profile,
            secret: String::new(),
            show_secret: false,
            has_saved_secret,
            is_new,
            tried_save: false,
            auth_tab,
            new_folder: None,
            users: Vec::new(),
            hosts: Vec::new(),
            user_ac: AutoField::new(egui::Id::new("editor_user_ac")),
            host_ac: AutoField::new(egui::Id::new("editor_host_ac")),
            test: Arc::new(Mutex::new(TestState::Idle)),
            key_search: None,
        }
    }

    /// Offers these usernames and hosts (most used first) while typing.
    pub fn with_suggestions(mut self, users: Vec<String>, hosts: Vec<String>) -> Self {
        self.users = users;
        self.hosts = hosts;
        self
    }

    /// [`Self::with_suggestions`] from the saved profiles.
    pub fn with_profiles(self, profiles: &[Profile]) -> Self {
        let (users, hosts) = suggest::profile_words(profiles);
        self.with_suggestions(users, hosts)
    }

    /// Selects a managed key (e.g. one just generated from this editor).
    pub fn use_key(&mut self, key: Uuid) {
        self.profile.auth = AuthMethod::Managed { key };
        self.auth_tab = AuthTab::Managed;
        self.has_saved_secret = secret::get(key).is_some();
    }

    pub fn show(&mut self, ctx: &egui::Context, folders: &FolderStore, keys: &KeyStore) -> EditorOutcome {
        let mut outcome = EditorOutcome::Open;
        let screen = ctx.content_rect();
        let width = (screen.width() - 48.0).clamp(480.0, MAX_WIDTH);
        let frame = egui::Frame::new().fill(theme::surface()).stroke(Stroke::new(1.0, theme::border())).corner_radius(16).shadow(egui::Shadow {
            offset: [0, 18],
            blur: 48,
            spread: 0,
            color: theme::shadow(),
        });
        let modal = egui::Modal::new(egui::Id::new("profile_editor")).frame(frame).show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::Frame::new().inner_margin(Margin { left: PAD, right: 18, top: 20, bottom: 18 }).show(ui, |ui| self.header(ui, &mut outcome));
            form::rule(ui);
            let body_h = (screen.height() - 250.0).max(240.0);
            egui::ScrollArea::vertical().max_height(body_h).auto_shrink([false, true]).show(ui, |ui| {
                egui::Frame::new().inner_margin(Margin::symmetric(PAD, 20)).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    form::section(ui, Some(Icon::Folder), "General", "How the host shows up in the sidebar", |ui| self.general(ui, folders));
                    ui.add_space(14.0);
                    form::section(ui, Some(Icon::Server), "Connection", "Where to connect and as whom", |ui| self.connection(ui));
                    ui.add_space(14.0);
                    self.auth_section(ui, keys, &mut outcome);
                    ui.add_space(14.0);
                    self.advanced(ui);
                });
            });
            form::rule(ui);
            egui::Frame::new().inner_margin(Margin::symmetric(PAD, 16)).show(ui, |ui| self.footer(ui, keys, &mut outcome));
        });
        if modal.should_close() && self.key_search.is_none() && matches!(outcome, EditorOutcome::Open) {
            outcome = EditorOutcome::Cancel;
        }
        outcome
    }

    /// Live preview: avatar in the host colour, name and address, close button.
    fn header(&self, ui: &mut Ui, outcome: &mut EditorOutcome) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 48.0), Sense::hover());
        let p = ui.painter();
        let accent = theme::hue_color(self.profile.hue);
        let avatar = Rect::from_min_size(rect.left_top(), Vec2::splat(48.0));
        p.rect_filled(avatar, CornerRadius::same(12), accent.gamma_multiply(0.2));
        p.rect_stroke(avatar, CornerRadius::same(12), Stroke::new(1.0, accent.gamma_multiply(0.45)), egui::StrokeKind::Inside);
        let name = self.profile.display_name();
        let initial = name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "+".into());
        p.text(avatar.center(), Align2::CENTER_CENTER, initial, FontId::proportional(20.0), accent);
        let x = avatar.right() + 14.0;
        let title = if self.is_new { "New host" } else { "Edit host" };
        p.text(Pos2::new(x, avatar.top() + 14.0), Align2::LEFT_CENTER, title, FontId::proportional(19.0), theme::text());
        let sub = if self.profile.host.trim().is_empty() { "Fill in the address below".to_string() } else { self.profile.address() };
        p.text(Pos2::new(x, avatar.bottom() - 11.0), Align2::LEFT_CENTER, sub, FontId::monospace(12.0), theme::text_dim());
        let close = Rect::from_center_size(Pos2::new(rect.right() - 16.0, rect.top() + 16.0), Vec2::splat(30.0));
        if ui.scope_builder(egui::UiBuilder::new().max_rect(close), |ui| w::close_button(ui, 30.0, false)).inner.on_hover_text("Close (Esc)").clicked() {
            *outcome = EditorOutcome::Cancel;
        }
    }

    fn general(&mut self, ui: &mut Ui, folders: &FolderStore) {
        let half = (ui.available_width() - form::GAP) / 2.0;
        form::row2(ui, half, |ui, lw, rw| {
            form::field(ui, lw, "Label", None, Some("Optional — the host name is used otherwise"), |ui| {
                form::text(ui, &mut self.profile.name, "My server", lw, false);
            });
            form::field(ui, rw, "Folder", None, None, |ui| self.folder_picker(ui, folders, rw));
        });
    }

    fn folder_picker(&mut self, ui: &mut Ui, folders: &FolderStore, width: f32) {
        if let Some(name) = &mut self.new_folder {
            let mut cancel = false;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                form::text(ui, name, "New folder name", width - 42.0, false);
                cancel = w::close_button(ui, 36.0, false).on_hover_text("Pick an existing folder").clicked();
            });
            form::note(ui, "Created when you save", false);
            if cancel {
                self.new_folder = None;
            }
            return;
        }
        let current = match self.profile.folder.and_then(|f| folders.get(f)) {
            Some(_) => folders.path_of(self.profile.folder.unwrap_or_default()),
            None => "Top level".into(),
        };
        let mut pick = None;
        let mut create = false;
        let chosen = self.profile.folder;
        form::select(ui, egui::Id::new("editor_folder"), width, &current, |ui| {
            if form::option(ui, "Top level", None, chosen.is_none()).clicked() {
                pick = Some(None);
            }
            for (id, depth) in folders.flattened() {
                let name = folders.get(id).map(|f| f.name.as_str()).unwrap_or_default();
                if form::option(ui, &format!("{}{name}", "      ".repeat(depth)), None, chosen == Some(id)).clicked() {
                    pick = Some(Some(id));
                }
            }
            ui.add_space(4.0);
            form::rule(ui);
            ui.add_space(4.0);
            if form::option(ui, "+  New folder…", None, false).clicked() {
                create = true;
            }
        });
        if let Some(f) = pick {
            self.profile.folder = f;
        }
        if create {
            self.new_folder = Some(String::new());
        }
    }

    fn connection(&mut self, ui: &mut Ui) {
        let host_err = self.tried_save && self.profile.host.trim().is_empty();
        let user_err = self.tried_save && self.profile.user.trim().is_empty();
        let port_err = self.port_error();
        let (hosts, users) = (&self.hosts, &self.users);
        let (host_ac, user_ac) = (&mut self.host_ac, &mut self.user_ac);
        let p = &mut self.profile;
        let port_text = &mut self.port_text;
        form::row2(ui, PORT_W, |ui, w, port_w| {
            let help = "Tip: paste user@host:port to fill everything";
            form::field(ui, w, "Host", host_err.then_some("Host is required"), Some(help), |ui| {
                let out = form::framed(ui, host_err, |ui| {
                    host_ac.show(
                        ui,
                        &mut p.host,
                        |t| form::style_edit(egui::TextEdit::singleline(t).hint_text("example.com or 10.0.0.5"), w),
                        |q| suggest::rank_words(q, hosts, suggest::SUGGEST_LIMIT),
                        egui::Order::Tooltip,
                    )
                });
                // Pasting `user@host:port` fills all three fields.
                if out.changed()
                    && p.host.contains('@')
                    && let Some(q) = Profile::parse_quick(&p.host)
                {
                    p.host = q.host;
                    p.user = q.user;
                    p.port = q.port;
                    *port_text = q.port.to_string();
                }
            });
            form::field(ui, port_w, "Port", port_err.then_some("1 – 65535"), None, |ui| {
                if form::text(ui, port_text, "22", port_w, port_err).changed() {
                    port_text.retain(|c| c.is_ascii_digit());
                    port_text.truncate(5);
                    p.port = port_text.parse().unwrap_or(0);
                }
            });
        });
        ui.add_space(8.0);
        let width = ui.available_width();
        form::field(ui, width, "Username", user_err.then_some("Username is required"), None, |ui| {
            form::framed(ui, user_err, |ui| {
                user_ac.show(
                    ui,
                    &mut p.user,
                    |t| form::style_edit(egui::TextEdit::singleline(t).hint_text("root"), width),
                    |q| suggest::rank_words(q, users, suggest::SUGGEST_LIMIT),
                    egui::Order::Tooltip,
                )
            });
        });
    }

    fn port_error(&self) -> bool {
        self.profile.port == 0 && (self.tried_save || !self.port_text.is_empty())
    }

    /// Collapsible card with the rarely changed settings.
    fn advanced(&mut self, ui: &mut Ui) {
        let id = egui::Id::new("editor_advanced");
        let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, false);
        let openness = state.openness(ui.ctx());
        egui::Frame::new()
            .fill(form::card_fill())
            .stroke(Stroke::new(1.0, theme::border().gamma_multiply(0.8)))
            .corner_radius(12)
            .inner_margin(Margin::same(18))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let top = ui.cursor().min;
                form::section_header(ui, Some(Icon::Gear), "Advanced", "Start folder, SFTP speed and colour", |ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
                    form::paint_chevron(ui.painter(), r.center(), openness, theme::text_dim());
                });
                let header = Rect::from_min_max(top, Pos2::new(ui.max_rect().right(), ui.cursor().min.y));
                let resp = ui.interact(header, id.with("header"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                if resp.clicked() {
                    state.toggle(ui);
                }
                state.show_body_unindented(ui, |ui| {
                    ui.add_space(14.0);
                    self.advanced_body(ui);
                });
            });
    }

    fn advanced_body(&mut self, ui: &mut Ui) {
        let p = &mut self.profile;
        let width = ui.available_width();
        form::field(ui, width, "Remote start folder", None, Some("Used when there's no last folder to resume"), |ui| {
            form::text(ui, &mut p.remote_start_dir, "home folder", width, false);
        });
        ui.add_space(10.0);
        form::field(ui, width, "SFTP pipeline", None, Some("Data kept in flight per file. Raise it on high-latency links."), |ui| {
            ui.horizontal(|ui| {
                let mut kb = p.pipeline_kb as f32;
                let spec =
                    w::SliderSpec { min: MIN_PIPELINE_KB as f32, max: MAX_PIPELINE_KB as f32, step: 0.0, log: true, format: &|v| format!("{} KB", v.round()) };
                if w::slider_with_value(ui, &mut kb, &spec).changed() {
                    p.pipeline_kb = (kb.round() as u32).clamp(MIN_PIPELINE_KB, MAX_PIPELINE_KB);
                }
            });
        });
        ui.add_space(10.0);
        form::field(ui, width, "Colour", None, None, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for hue in SWATCHES {
                    if swatch(ui, hue, (p.hue - hue).abs() < 1.0).on_hover_text("Use this colour").clicked() {
                        p.hue = hue;
                    }
                }
                ui.add_space(8.0);
                let spec = w::SliderSpec { min: 0.0, max: 359.0, step: 1.0, log: false, format: &|v| format!("{v:.0}°") };
                w::slider(ui, &mut p.hue, &spec).on_hover_text("Any hue");
            });
        });
    }

    fn footer(&mut self, ui: &mut Ui, keys: &KeyStore, outcome: &mut EditorOutcome) {
        let state = self.test.lock().map(|s| s.clone()).unwrap_or(TestState::Idle);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let running = matches!(state, TestState::Running(_));
            if form::button(ui, "Test connection", Kind::Secondary, !running).on_hover_text("Connect and log in once, without saving").clicked() {
                self.start_test(keys, outcome);
            }
            test_status(ui, &state);
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let save = form::button(ui, "Save", Kind::Primary, true).on_hover_text("Enter");
                if form::button(ui, "Cancel", Kind::Ghost, true).on_hover_text("Esc").clicked() {
                    *outcome = EditorOutcome::Cancel;
                }
                if matches!(state, TestState::Idle) && ui.available_width() > 190.0 {
                    form::hints(ui, &[("Enter", "save"), ("Esc", "close")]);
                }
                if save.clicked() || self.enter_pressed(ui) {
                    self.save(keys, outcome);
                }
            });
        });
        if let TestState::Failed(e) = &state {
            ui.add_space(8.0);
            ui.add(egui::Label::new(RichText::new(e).size(12.0).color(theme::danger())).wrap());
        }
    }

    /// Plain (or Ctrl+) Enter saves, unless a suggestion list or dropdown is taking it.
    fn enter_pressed(&self, ui: &Ui) -> bool {
        let busy = self.host_ac.is_open() || self.user_ac.is_open() || egui::Popup::is_any_open(ui.ctx());
        !busy && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift && !i.modifiers.alt)
    }

    fn start_test(&mut self, keys: &KeyStore, outcome: &mut EditorOutcome) {
        match self.validated(keys) {
            Ok(()) => {
                let secret = (!self.secret.is_empty()).then(|| self.secret.clone()).or_else(|| secret::get(self.profile.secret_id()));
                *outcome = EditorOutcome::Test(Box::new(self.profile.clone()), secret, self.test.clone());
            }
            Err(e) => {
                self.tried_save = true;
                if let Ok(mut s) = self.test.lock() {
                    *s = TestState::Failed(e);
                }
            }
        }
    }

    fn save(&mut self, keys: &KeyStore, outcome: &mut EditorOutcome) {
        self.tried_save = true;
        if self.validated(keys).is_ok() {
            let secret = (!self.secret.is_empty()).then(|| self.secret.clone());
            let new_folder = self.new_folder.clone().filter(|n| !n.trim().is_empty());
            *outcome = EditorOutcome::Save(Box::new(SaveRequest { profile: self.profile.clone(), secret, new_folder }));
        }
    }

    fn validated(&self, keys: &KeyStore) -> Result<(), String> {
        self.profile.validate()?;
        if let AuthMethod::Managed { key } = self.profile.auth
            && keys.get(key).is_none()
        {
            return Err("Choose a saved key (or generate one)".into());
        }
        Ok(())
    }
}

fn swatch(ui: &mut Ui, hue: f32, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() || selected, theme::anim_time(0.1));
    let p = ui.painter();
    if selected {
        p.circle_stroke(rect.center(), 12.0, Stroke::new(2.0, theme::hue_color(hue)));
    }
    p.circle_filled(rect.center(), 8.0 + 1.0 * h, theme::hue_color(hue));
    resp
}

fn test_status(ui: &mut Ui, state: &TestState) {
    match state {
        TestState::Idle | TestState::Failed(_) => {}
        TestState::Running(stage) => {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
            loading::mini_spinner(ui, rect.center(), 6.0, theme::accent());
            ui.label(RichText::new(format!("{}…", stage.label())).size(12.5).color(theme::text_dim()));
        }
        TestState::Ok(ms) => {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
            w::paint_icon(ui.painter(), rect.shrink(2.0), Icon::Check, theme::accent());
            ui.label(RichText::new(format!("Connected in {ms} ms")).size(12.5).color(theme::accent()));
        }
    }
}

fn default_key_path() -> PathBuf {
    let Some(dirs) = directories::UserDirs::new() else { return PathBuf::new() };
    let ssh = dirs.home_dir().join(".ssh");
    ["id_ed25519", "id_ecdsa", "id_rsa"].iter().map(|n| ssh.join(n)).find(|p| p.exists()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasting_quick_connect_is_parsed_once_saved() {
        let mut e = ProfileEditor::new(Profile::new(), true);
        e.profile.host = "h".into();
        e.profile.user = "u".into();
        let keys = KeyStore::default();
        assert!(e.validated(&keys).is_ok());
        e.profile.auth = AuthMethod::Managed { key: Uuid::new_v4() };
        assert!(e.validated(&keys).is_err(), "unknown managed key");
        assert_eq!(e.profile.port, crate::profile::DEFAULT_PORT);
        assert_eq!(e.port_text, crate::profile::DEFAULT_PORT.to_string());
    }

    #[test]
    fn use_key_switches_to_saved_key_tab() {
        let mut e = ProfileEditor::new(Profile::new(), true);
        let id = Uuid::new_v4();
        e.use_key(id);
        assert_eq!(e.auth_tab, AuthTab::Managed);
        assert_eq!(e.profile.auth, AuthMethod::Managed { key: id });
    }

    #[test]
    fn empty_port_is_an_error_only_when_typed_or_saving() {
        let mut e = ProfileEditor::new(Profile::new(), true);
        assert!(!e.port_error());
        e.profile.port = 0;
        e.port_text.clear();
        assert!(!e.port_error(), "not yet");
        e.tried_save = true;
        assert!(e.port_error());
    }
}
