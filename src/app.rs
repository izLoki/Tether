//! Application state and the frame loop: sidebar, dock workspace, modals, toasts.
//! Sidebar actions, the host editor and the key manager live in `app_actions.rs`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Key, RichText};
use egui_dock::{DockArea, DockState, Node, Split};
use uuid::Uuid;

#[path = "app_actions.rs"]
mod actions;
#[path = "app_agents.rs"]
mod agents;
#[path = "app_chrome.rs"]
mod chrome;
#[path = "app_layout.rs"]
mod layout_ui;
#[path = "app_local.rs"]
mod local;
#[path = "app_palette.rs"]
mod palette_ui;
#[path = "app_views.rs"]
mod views_ui;

use crate::folders::FolderStore;
use crate::keybindings::{self, Action};
use crate::keystore::KeyStore;
use crate::known_hosts::KnownHosts;
use crate::profile::{AuthMethod, Profile, ProfileStore, secret};
use crate::ssh::connection::Stage;
use crate::ssh::pool::Pool;
use crate::ssh::terminal::{LinkState, TerminalHandle, run_shell};
use crate::theme;
use crate::ui::detach::Detacher;
use crate::ui::editor::ProfileEditor;
use crate::ui::import_view::{ImportDialog, ImportOutcome};
use crate::ui::key_manager::KeyManager;
use crate::ui::palette::Palette;
use crate::ui::settings_view::SettingsView;
use crate::ui::sftp_view::SftpView;
use crate::ui::sidebar::{OpenKind, Sidebar, WorkspaceInfo};
use crate::ui::tabs::{Tab, TabBody, Viewer};
use crate::ui::terminal_view::TerminalView;
use crate::ui::toast::Toasts;
use crate::ui::vertical_tabs::VerticalTabs;
use crate::workspace::{self, LAST_SESSION, TabKind, Workspaces};

/// Password / passphrase asked before connecting (nothing stored, no live connection).
struct Prompt {
    profile: Profile,
    action: PromptAction,
    secret: String,
    remember: bool,
}

enum PromptAction {
    Open(OpenKind),
    Retry(u64),
}

pub struct TetherApp {
    rt: tokio::runtime::Runtime,
    pool: Pool,
    store: ProfileStore,
    folders: FolderStore,
    keys: KeyStore,
    workspaces: Workspaces,
    dock: DockState<Tab>,
    sidebar: Sidebar,
    /// Rename state of the vertical tab list (inline editing).
    vtabs: VerticalTabs,
    /// Where a dragged activity icon can land (rebuilt every frame).
    view_targets: crate::ui::activity::DropTargets,
    palette: Option<Palette>,
    /// A tab shown alone over the whole workspace (maximized pane).
    zoomed: Option<u64>,
    /// Give this tab's terminal the keyboard on the next frame.
    focus_request: Option<u64>,
    /// Terminal widget ids seen last frame (see `PaneFx::terminal_ids`).
    terminal_ids: Vec<egui::Id>,
    /// "Rename tab" dialog: the tab and the edited name.
    rename: Option<(u64, String)>,
    /// Tabs of the main dock last frame (a new one in the focused pane gets the keyboard).
    seen_tabs: HashSet<u64>,
    broadcast: bool,
    editor: Option<ProfileEditor>,
    import: Option<ImportDialog>,
    custom_boot: Option<crate::ui::agents_panel::CustomBoot>,
    project_cwds: std::collections::HashMap<u64, String>,
    settings_view: Option<SettingsView>,
    key_manager: Option<KeyManager>,
    prompt: Option<Prompt>,
    confirm_delete: Option<Uuid>,
    toasts: Toasts,
    last_layout: Vec<workspace::TabSpec>,
    live_profiles: HashSet<Uuid>,
    /// Profiles with open tabs, shared with pending disconnect tasks (see `track_layout`).
    in_use: Arc<Mutex<HashSet<Uuid>>>,
    focused_terminal: Option<u64>,
    /// Tabs dragged out into their own native windows (see `ui::detach`).
    detached: Detacher,
    /// Minimized pop-up windows and the movable mini-dock bar with Genie animations.
    mini_dock: crate::ui::mini_dock::MiniDock,
    /// AI agent sessions and the resource monitor (`app_agents.rs`).
    agents: crate::agents::AgentHub,
}

/// `TETHER_CONFIG_DIR` overrides the location (portable installs, tests).
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("TETHER_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    directories::ProjectDirs::from("dev", "tether", "tether").map(|d| d.config_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from(".tether"))
}

impl TetherApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> anyhow::Result<Self> {
        let dir = config_dir();
        crate::settings::init(dir.join("settings.json"));
        crate::themes::apply_saved(&cc.egui_ctx);
        crate::updater::check_on_startup(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        crate::pty::shells::warm_up(move || ctx.request_repaint());
        crate::agents::launch::warm_up();
        let toasts = Toasts::default();
        if let Some((summary, path)) = crate::crashlog::take_last_crash_summary() {
            toasts.error(format!("Previous session crashed: {summary}\nCrash log saved to {}", path.display()));
        }
        let mut store = ProfileStore::load(dir.join("profiles.json")).unwrap_or_else(|e| {
            toasts.error(format!("Could not read profiles: {e:#}"));
            ProfileStore::default()
        });
        let mut folders = FolderStore::load(dir.join("folders.json")).unwrap_or_else(|e| {
            toasts.error(format!("Could not read folders: {e:#}"));
            FolderStore::default()
        });
        // Older versions grouped hosts by a free-text name: turn those into folders.
        if folders.migrate_groups(&mut store.profiles) && (folders.save().is_err() || store.save().is_err()) {
            toasts.error("Could not save the migrated host folders");
        }
        let keys = KeyStore::load(crate::keystore::default_dir()).unwrap_or_else(|e| {
            toasts.error(format!("Could not read the key manager index: {e:#}"));
            KeyStore::default()
        });
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("tether-io").build()?;
        let agents = crate::agents::AgentHub::new(rt.handle(), &cc.egui_ctx);
        Ok(Self {
            rt,
            pool: Pool::new(KnownHosts::load(dir.join("known_hosts.json"))),
            store,
            folders,
            keys,
            workspaces: Workspaces::load(dir.join("workspaces.json")),
            dock: DockState::new(Vec::new()),
            sidebar: Sidebar::default(),
            vtabs: VerticalTabs::default(),
            view_targets: Default::default(),
            palette: None,
            zoomed: None,
            focus_request: None,
            terminal_ids: Vec::new(),
            rename: None,
            seen_tabs: HashSet::new(),
            broadcast: false,
            editor: None,
            import: None,
            settings_view: None,
            key_manager: None,
            prompt: None,
            confirm_delete: None,
            toasts,
            last_layout: Vec::new(),
            live_profiles: HashSet::new(),
            in_use: Arc::default(),
            focused_terminal: None,
            detached: Detacher::default(),
            mini_dock: Default::default(),
            agents,
            custom_boot: None,
            project_cwds: Default::default(),
        })
    }

    // ---- opening sessions -------------------------------------------------------

    /// Whether connecting needs a password / passphrase the user hasn't stored.
    fn needs_secret(&self, p: &Profile) -> bool {
        if self.pool.has_live(p.id) {
            return false;
        }
        match &p.auth {
            AuthMethod::Password => secret::get(p.id).is_none(),
            AuthMethod::Managed { key } => self.keys.get(*key).is_some_and(|k| k.encrypted) && secret::get(*key).is_none(),
            AuthMethod::Key { .. } => false,
        }
    }

    fn request_open(&mut self, profile: Profile, kind: OpenKind, ctx: &egui::Context) {
        if self.needs_secret(&profile) {
            self.prompt = Some(Prompt { profile, action: PromptAction::Open(kind), secret: String::new(), remember: true });
            return;
        }
        let secret = secret::get(profile.secret_id());
        self.open_now(profile, kind, secret, ctx);
    }

    fn open_now(&mut self, profile: Profile, kind: OpenKind, secret: Option<String>, ctx: &egui::Context) {
        self.store.touch(profile.id);
        let _ = self.store.save();
        match kind {
            OpenKind::Terminal => {
                let tab = self.terminal_tab(&profile, secret, ctx);
                self.dock.push_to_focused_leaf(tab);
            }
            OpenKind::Sftp => {
                let can_attach = self.dock.find_active_focused().is_some_and(|(_, active)| {
                    !active.is_local() && active.profile.id == profile.id && matches!(active.body, TabBody::Terminal(_)) && active.linked_sftp.is_none()
                });
                if can_attach {
                    let view = self.spawn_sftp(&profile, secret, ctx);
                    if let Some((_, active)) = self.dock.find_active_focused() {
                        active.linked_sftp = Some(crate::ui::tabs::LinkedSftp::new(view));
                    }
                    return;
                }
                let tab = self.sftp_tab(&profile, secret, ctx);
                self.dock.push_to_focused_leaf(tab);
            }
            OpenKind::Both => {
                let mut term = self.terminal_tab(&profile, secret.clone(), ctx);
                let sftp_view = self.spawn_sftp(&profile, secret, ctx);
                term.linked_sftp = Some(crate::ui::tabs::LinkedSftp::new(sftp_view));
                self.dock.push_to_focused_leaf(term);
            }
        }
    }

    fn split_focused(&mut self, tab: Tab, split: Split) {
        match self.dock.focused_leaf() {
            Some(path) => {
                self.dock.split(path, split, 0.5, Node::leaf_with(vec![tab]));
            }
            None => self.dock.push_to_focused_leaf(tab),
        }
    }

    fn spawn_terminal(&self, profile: &Profile, secret: Option<String>, ctx: &egui::Context) -> TerminalView {
        let image_target = crate::ui::attachments::RemoteTarget { pool: self.pool.clone(), profile: profile.clone(), runtime: self.rt.handle().clone() };
        let (handle, rx) = TerminalHandle::new(24, 80);
        // Wakes the UI even if this tab is never drawn (restored workspace, background tab).
        handle.repaint.attach(ctx);
        let (pool, profile, h, ctx, toasts) = (self.pool.clone(), profile.clone(), handle.clone(), ctx.clone(), self.toasts.clone());
        self.rt.spawn(async move {
            let repaint = {
                let ctx = ctx.clone();
                move || ctx.request_repaint()
            };
            let on_stage = {
                let (h, ctx) = (h.clone(), ctx.clone());
                move |stage: Stage| {
                    h.set_stage(stage);
                    ctx.request_repaint();
                }
            };
            match pool.get_staged(&profile, secret, &on_stage).await {
                Ok(conn) => {
                    if let Some(fp) = conn.learned_fingerprint.lock().ok().and_then(|mut f| f.take()) {
                        toasts.info(format!("New host key trusted for {}\n{fp}", profile.host));
                    }
                    if let Err(e) = run_shell(conn, h.clone(), rx, repaint).await {
                        h.set_state(LinkState::Failed(format!("{e:#}")));
                    }
                }
                Err(e) => h.set_state(LinkState::Failed(format!("{e:#}"))),
            }
            ctx.request_repaint();
        });
        let mut view = TerminalView::new(handle);
        view.attachments().remote = Some(image_target);
        view
    }

    fn terminal_tab(&self, profile: &Profile, secret: Option<String>, ctx: &egui::Context) -> Tab {
        Tab::new(profile.clone(), TabBody::Terminal(Box::new(self.spawn_terminal(profile, secret, ctx))))
    }

    fn sftp_tab(&self, profile: &Profile, secret: Option<String>, ctx: &egui::Context) -> Tab {
        let view = self.spawn_sftp(profile, secret, ctx);
        Tab::new(profile.clone(), TabBody::Sftp(Box::new(view)))
    }

    fn spawn_sftp(&self, profile: &Profile, secret: Option<String>, ctx: &egui::Context) -> SftpView {
        let view = SftpView::new(profile.clone(), self.rt.handle().clone(), ctx.clone());
        view.connect(self.pool.clone(), secret, self.toasts.clone());
        view
    }

    /// `Ctrl+Shift+F`: links an SFTP browser to the right side of the focused SSH terminal
    /// tab (or closes it if already open on that terminal).
    pub(super) fn open_linked_sftp(&mut self, ctx: &egui::Context) {
        let Some((_, tab)) = self.dock.find_active_focused() else { return };
        if tab.is_local() {
            return;
        }
        if matches!(tab.body, TabBody::Terminal(_)) {
            if tab.linked_sftp.is_some() {
                tab.linked_sftp = None;
                return;
            }
            let uid = tab.uid;
            let _ = self.ensure_linked_sftp(uid, ctx);
            return;
        }
        self.duplicate_focused(Split::Right, TabKind::Sftp, ctx);
    }

    fn reconnect(&mut self, uid: u64, secret_override: Option<String>, ctx: &egui::Context) {
        if self.all_tabs().any(|t| t.uid == uid && t.is_local()) {
            self.respawn_local(uid, ctx);
            return;
        }
        let Some((profile, is_terminal)) = self.all_tabs().find(|t| t.uid == uid).map(|t| (t.profile.clone(), matches!(t.body, TabBody::Terminal(_)))) else {
            return;
        };
        let profile = self.store.get(profile.id).cloned().unwrap_or(profile);
        if secret_override.is_none() && self.needs_secret(&profile) {
            self.prompt = Some(Prompt { profile, action: PromptAction::Retry(uid), secret: String::new(), remember: true });
            return;
        }
        let secret = secret_override.or_else(|| secret::get(profile.secret_id()));
        let new_view = is_terminal.then(|| self.spawn_terminal(&profile, secret.clone(), ctx));
        let Some(tab) = self.dock.iter_all_tabs_mut().map(|(_, t)| t).chain(self.detached.tabs_mut()).chain(self.mini_dock.tabs_mut()).find(|t| t.uid == uid)
        else {
            return;
        };
        tab.profile = profile;
        match (&mut tab.body, new_view) {
            (TabBody::Terminal(view), Some(v)) => {
                **view = v;
                if let Some(linked) = &mut tab.linked_sftp {
                    linked.view.profile = tab.profile.clone();
                    linked.view.connect(self.pool.clone(), secret, self.toasts.clone());
                }
            }
            (TabBody::Sftp(view), _) => {
                view.profile = tab.profile.clone();
                view.connect(self.pool.clone(), secret, self.toasts.clone());
            }
            _ => {}
        }
    }

    fn duplicate_focused(&mut self, split: Split, kind: TabKind, ctx: &egui::Context) {
        let Some((_, tab)) = self.dock.find_active_focused() else { return };
        if let Some((spec, cwd)) = local::origin(tab) {
            // A local shell has no files pane; splits run the same shell again.
            if kind == TabKind::Terminal {
                let tab = self.local_tab(&spec, cwd, ctx);
                self.split_focused(tab, split);
            }
            return;
        }
        let remote_cwd = tab.current_cwd();
        let profile = tab.profile.clone();
        let secret = secret::get(profile.secret_id());
        let mut new_tab = match kind {
            TabKind::Sftp => self.sftp_tab(&profile, secret, ctx),
            TabKind::Terminal | TabKind::Local { .. } => self.terminal_tab(&profile, secret, ctx),
            TabKind::Unknown => return,
        };
        if let (Some(dir), TabBody::Terminal(view)) = (remote_cwd, &mut new_tab.body) {
            send_initial_remote_cd(view, &dir);
        }
        self.split_focused(new_tab, split);
    }

    /// Closes the focused tab the way its close button would (editors may refuse).
    fn close_focused(&mut self) {
        let Some((_, tab)) = self.dock.find_active_focused() else { return };
        let close = match &mut tab.body {
            TabBody::Terminal(t) => {
                t.handle.close();
                true
            }
            TabBody::Editor(e) => e.request_close(),
            TabBody::Sftp(_) => true,
        };
        let uid = tab.uid;
        if close && let Some(path) = self.dock.find_tab_from(|t| t.uid == uid) {
            self.dock.remove_tab(path);
        }
    }

    /// Activates the next (or previous) tab of the focused pane, wrapping around.
    fn cycle_tab(&mut self, forward: bool) {
        let Some(path) = self.dock.focused_leaf() else { return };
        let Ok(leaf) = self.dock.leaf_mut(path) else { return };
        let n = leaf.tabs.len();
        if n < 2 {
            return;
        }
        let next = if forward { (leaf.active.0 + 1) % n } else { (leaf.active.0 + n - 1) % n };
        let _ = leaf.set_active_tab(next);
    }

    // ---- detached windows ---------------------------------------------------------

    /// Tabs of the main dock, every detached window, and the mini-dock bar. Anything
    /// that must see every open tab (connection tracking, counts, lookups) iterates this.
    fn all_tabs(&self) -> impl Iterator<Item = &Tab> {
        self.dock.iter_all_tabs().map(|(_, t)| t).chain(self.detached.tabs()).chain(self.mini_dock.tabs())
    }

    /// Shows the native windows of detached tabs and applies their tab output like
    /// `workspace_area` does for the main dock.
    fn detached_windows(&mut self, ctx: &egui::Context) {
        let mut viewer = Viewer { time: ctx.input(|i| i.time), ..Viewer::default() };
        self.detached.frame(ctx, &mut self.dock, &mut viewer, &mut self.mini_dock);
        if viewer.focused_uid.is_some() {
            self.focused_terminal = viewer.focused_uid;
        }
        self.apply_linked_sftp_actions(std::mem::take(&mut viewer.linked_sftp_actions), ctx);
        self.route_input(viewer.inputs);
        for uid in viewer.reconnect {
            self.reconnect(uid, None, ctx);
        }
        for (uid, path) in viewer.open_folders {
            self.open_terminal_folder(uid, path, ctx);
        }
        for (uid, files) in viewer.open_files {
            self.open_terminal_files(uid, files, ctx);
        }
    }

    // ---- workspaces -------------------------------------------------------------

    fn layout_specs(&self) -> Option<workspace::Layout<workspace::TabSpec>> {
        // Editor tabs hold unsaved buffers, not layout: never restore them as panes.
        workspace::capture(&self.dock, |t: &Tab| t.is_persistable().then(|| t.spec()))
    }

    /// Saved layouts as the sidebar shows them (tab count and host names).
    fn workspace_infos(&self) -> Vec<WorkspaceInfo> {
        self.workspaces
            .names()
            .into_iter()
            .filter_map(|name| {
                let mut tabs = self.workspaces.get(&name)?.tabs();
                tabs.retain(|t| t.kind != TabKind::Unknown);
                let mut hosts: Vec<String> = Vec::new();
                for spec in &tabs {
                    let name = match &spec.kind {
                        // Per frame: never wait on shell detection (the id stands in meanwhile).
                        TabKind::Local { shell_id } => Some(crate::pty::shells::find_detected(shell_id).map_or_else(|| shell_id.clone(), |s| s.name.clone())),
                        _ => self.store.get(spec.profile_id).map(|p| p.display_name().to_string()),
                    };
                    if let Some(name) = name
                        && !hosts.contains(&name)
                    {
                        hosts.push(name);
                    }
                }
                Some(WorkspaceInfo { name, tabs: tabs.len(), hosts })
            })
            .collect()
    }

    fn restore_workspace(&mut self, name: &str, ctx: &egui::Context) {
        let Some(layout) = self.workspaces.get(name).cloned() else { return };
        let restored = workspace::rebuild(&layout, &mut |spec| {
            let mut tab = if let TabKind::Local { shell_id } = &spec.kind {
                let cwd = spec.cwd.as_deref().map(PathBuf::from);
                crate::pty::shells::find(shell_id).map(|shell| self.local_tab(shell, cwd, ctx))?
            } else {
                let profile = self.store.get(spec.profile_id)?.clone();
                let secret = secret::get(profile.secret_id());
                match spec.kind {
                    TabKind::Sftp => {
                        let mut tab = self.sftp_tab(&profile, secret, ctx);
                        if let TabBody::Sftp(view) = &mut tab.body {
                            view.restore_local_state(spec.sftp_local_cwd.as_deref(), spec.sftp_show_local);
                        }
                        tab
                    }
                    TabKind::Terminal | TabKind::Local { .. } => {
                        let mut term = self.terminal_tab(&profile, secret.clone(), ctx);
                        if let (Some(dir), TabBody::Terminal(view)) = (&spec.cwd, &mut term.body) {
                            send_initial_remote_cd(view, dir);
                        }
                        if spec.with_sftp {
                            let mut sftp_view = self.spawn_sftp(&profile, secret, ctx);
                            sftp_view.restore_local_state(spec.sftp_local_cwd.as_deref(), spec.sftp_show_local);
                            term.linked_sftp = Some(crate::ui::tabs::LinkedSftp::new(sftp_view));
                        }
                        term
                    }
                    TabKind::Unknown => return None,
                }
            };
            tab.rename(spec.title.clone());
            Some(tab)
        });
        self.zoomed = None;
        for (_, tab) in self.dock.iter_all_tabs() {
            if let TabBody::Terminal(t) = &tab.body {
                t.handle.close();
            }
        }
        self.dock = restored;
        self.toasts.success(format!("Workspace “{name}” restored"));
    }

    /// Keeps "Last session" current and closes transports nobody uses any more.
    fn track_layout(&mut self) {
        let layout: Vec<workspace::TabSpec> = self.all_tabs().map(|t| t.spec()).collect();
        if layout != self.last_layout {
            if !layout.is_empty() {
                let _ = self.workspaces.put(LAST_SESSION.into(), self.layout_specs());
            }
            self.last_layout = layout;
        }
        // Local shells (nil id) hold no pooled connection.
        let in_use: HashSet<Uuid> = self.last_layout.iter().map(|s| s.profile_id).filter(|id| !id.is_nil()).collect();
        if let Ok(mut shared) = self.in_use.lock() {
            shared.clone_from(&in_use);
        }
        for gone in self.live_profiles.difference(&in_use) {
            let (pool, id, shared) = (self.pool.clone(), *gone, self.in_use.clone());
            // Re-checked under the pool slot lock: the profile may be reopened before this runs.
            self.rt.spawn(async move { pool.drop_connection(id, || shared.lock().map(|s| !s.contains(&id)).unwrap_or(false)).await });
        }
        self.live_profiles = in_use;
    }

    // ---- frame --------------------------------------------------------------------

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        for a in Action::ALL {
            if palette_ui::is_global(a) && keybindings::consume(ctx, a) {
                self.run_action(a, ctx);
            }
        }
    }

    fn workspace_area(&mut self, ui: &mut egui::Ui) {
        // Translucent window: the panel must not stack its fill under the terminals.
        let see_through = crate::themes::see_through() && self.dock.iter_all_tabs().next().is_some();
        let fill = if see_through { egui::Color32::TRANSPARENT } else { theme::bg() };
        let vertical = crate::settings::snapshot().layout.tab_bars_hidden();
        let margin = if vertical { egui::Margin { left: 0, right: 0, top: 0, bottom: 6 } } else { egui::Margin { left: 0, right: 6, top: 0, bottom: 6 } };
        let frame = egui::Frame::new().fill(fill).inner_margin(margin);
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            let area = ui.available_rect_before_wrap();
            if self.dock.iter_all_tabs().next().is_none() && self.mini_dock.is_empty() {
                self.zoomed = None;
                self.welcome(ui);
                return;
            }
            if self.dock.iter_all_tabs().next().is_none() {
                self.zoomed = None;
                self.welcome(ui);
                self.mini_dock.frame(ui.ctx(), &mut self.dock, area);
                return;
            }
            self.absorb_same_server_sftp_splits();
            let mut style = theme::dock_style(ui.style());
            if see_through {
                style.tab.tab_body.bg_fill = theme::surface().gamma_multiply(crate::themes::window_opacity());
                crate::ui::backdrop::paint_frame(ui, egui::vec2(6.0, 6.0));
            }
            let hide_bars = layout_ui::sync_tab_bars(&mut self.dock);
            if hide_bars {
                // Vertical tabs: no strip, no reveal triangle, no drag margin.
                style.buttons.show_tab_bar_size = 0.0;
                style.tab.tab_body.hidden_tab_bar_drag_height = Some(0.0);
            }
            self.focus_new_tab();
            let mut viewer = Viewer::with_panes(self.pane_fx(ui.ctx()));
            viewer.time = ui.input(|i| i.time);
            crate::ui::tabs::apply_commands(&mut self.dock, self.rt.handle(), ui.ctx());
            if !self.zoomed_view(ui, &mut viewer) {
                DockArea::new(&mut self.dock)
                    .style(style)
                    // Floating windows may leave the app: `Detacher` turns them into native windows.
                    .window_bounds(crate::ui::detach_geom::floating_bounds(ui.ctx().content_rect()))
                    // Each pane's "+" lists the local shells (`Viewer::add_popup`).
                    .show_add_buttons(true)
                    .show_add_popup(true)
                    .hidable_tab_bars(hide_bars)
                    .show_leaf_collapse_buttons(false)
                    .show_leaf_close_all_buttons(false)
                    .show_inside(ui, &mut viewer);
                // A row of the vertical tab list dropped onto the panes.
                if let Some(action) = crate::ui::vertical_tabs::drop_on_panes(ui.ctx(), &self.dock, area) {
                    viewer.tab_actions.push(action);
                }
            }
            self.mini_dock.frame(ui.ctx(), &mut self.dock, area);
            self.after_views(&mut viewer);
            self.open_local_in(std::mem::take(&mut viewer.new_local), ui.ctx());
            self.open_agents_in(std::mem::take(&mut viewer.new_agent), ui.ctx());
            if viewer.focused_uid.is_some() {
                self.focused_terminal = viewer.focused_uid;
            }
            self.apply_linked_sftp_actions(std::mem::take(&mut viewer.linked_sftp_actions), ui.ctx());
            self.apply_tab_actions(std::mem::take(&mut viewer.tab_actions), ui.ctx());
            self.route_input(viewer.inputs);
            for uid in viewer.reconnect {
                self.reconnect(uid, None, ui.ctx());
            }
            for (uid, path) in viewer.open_folders {
                self.open_terminal_folder(uid, path, ui.ctx());
            }
            for (uid, files) in viewer.open_files {
                self.open_terminal_files(uid, files, ui.ctx());
            }
        });
    }

    /// Handles separating (`Detach`) or minimizing (`Minimize`) a terminal tab's linked
    /// right-hand SFTP pane when dragged or clicked in its header.
    fn apply_linked_sftp_actions(&mut self, actions: Vec<crate::ui::tabs::LinkedSftpAction>, ctx: &egui::Context) {
        use crate::ui::tabs::LinkedSftpAction;
        for action in actions {
            match action {
                LinkedSftpAction::Detach { uid, pos, rect } => {
                    let Some(tab) = self.dock.iter_all_tabs_mut().map(|(_, t)| t).chain(self.detached.tabs_mut()).find(|t| t.uid == uid) else {
                        continue;
                    };
                    let Some(linked) = tab.linked_sftp.take() else { continue };
                    let sftp_tab = Tab::new(tab.profile.clone(), TabBody::Sftp(linked.view));
                    let size = rect.size().max(egui::vec2(420.0, 320.0));
                    let min = pos.map(|p| p - egui::vec2(size.x * 0.4, 14.0)).unwrap_or(rect.min);
                    let surf = self.dock.add_window(vec![sftp_tab]);
                    if let Some(state) = self.dock.get_window_state_mut(surf) {
                        state.set_position(min).set_size(size);
                    }
                    self.last_layout.clear();
                }
                LinkedSftpAction::Minimize { uid, rect } => {
                    let Some(tab) = self.dock.iter_all_tabs_mut().map(|(_, t)| t).chain(self.detached.tabs_mut()).find(|t| t.uid == uid) else {
                        continue;
                    };
                    let Some(linked) = tab.linked_sftp.take() else { continue };
                    let sftp_tab = Tab::new(tab.profile.clone(), TabBody::Sftp(linked.view));
                    self.mini_dock.minimize_tabs(ctx, vec![sftp_tab], rect);
                    self.last_layout.clear();
                }
            }
        }
    }

    /// When a same-server SFTP pane sits in a main-surface split right beside its SSH
    /// terminal, links it directly into that SSH terminal tab (`linked_sftp`) so switching
    /// away to another terminal hides the right-hand SFTP automatically.
    fn absorb_same_server_sftp_splits(&mut self) {
        let mut pairs: Vec<(u64, u64)> = Vec::new();
        let leaves: Vec<_> = self.dock.iter_leaves().filter(|(p, l)| p.surface.is_main() && !l.tabs.is_empty()).collect();
        if leaves.len() < 2 {
            return;
        }
        for i in 0..leaves.len() {
            let (_, left_leaf) = leaves[i];
            let Some(active_term) = left_leaf.tabs.get(left_leaf.active.0) else { continue };
            if active_term.is_local() || !matches!(active_term.body, TabBody::Terminal(_)) || active_term.linked_sftp.is_some() {
                continue;
            }
            for (j, (_, right_leaf)) in leaves.iter().enumerate() {
                if i == j || right_leaf.tabs.len() != 1 {
                    continue;
                }
                let sftp_tab = &right_leaf.tabs[0];
                if matches!(sftp_tab.body, TabBody::Sftp(_)) && sftp_tab.profile.id == active_term.profile.id {
                    pairs.push((active_term.uid, sftp_tab.uid));
                    break;
                }
            }
        }
        for (term_uid, sftp_uid) in pairs {
            let Some(sftp_path) = self.dock.find_tab_from(|t| t.uid == sftp_uid) else { continue };
            let Some(sftp_tab) = self.dock.remove_tab(sftp_path) else { continue };
            let TabBody::Sftp(sftp_view) = sftp_tab.body else { continue };
            if let Some(term_tab) = self.dock.iter_all_tabs_mut().map(|(_, t)| t).find(|t| t.uid == term_uid) {
                term_tab.linked_sftp = Some(crate::ui::tabs::LinkedSftp { view: sftp_view, split_fraction: 0.54 });
                self.last_layout.clear();
            }
        }
    }

    fn route_input(&mut self, inputs: Vec<(u64, Vec<u8>)>) {
        for (uid, bytes) in inputs {
            self.note_agent_input(uid);
            for tab in self.dock.iter_all_tabs().map(|(_, t)| t).chain(self.detached.tabs()) {
                if let TabBody::Terminal(t) = &tab.body
                    && (tab.uid == uid || self.broadcast)
                {
                    t.handle.send(bytes.clone());
                }
            }
        }
    }

    /// Both the shortcut and path links use this companion pane, owned by the source terminal.
    fn ensure_linked_sftp(&mut self, uid: u64, ctx: &egui::Context) -> Option<&mut SftpView> {
        let (profile, terminal, already_open) = self.all_tabs().find_map(|t| match &t.body {
            TabBody::Terminal(term) if t.uid == uid && !t.is_local() => Some((t.profile.clone(), term.handle.clone(), t.linked_sftp.is_some())),
            _ => None,
        })?;
        let new_view = if already_open {
            None
        } else {
            let profile = self.store.get(profile.id).cloned().unwrap_or(profile);
            Some(self.spawn_sftp(&profile, secret::get(profile.secret_id()), ctx))
        };
        let tab = self.dock.iter_all_tabs_mut().map(|(_, t)| t).chain(self.detached.tabs_mut()).chain(self.mini_dock.tabs_mut()).find(|t| t.uid == uid)?;
        if let Some(view) = new_view {
            tab.linked_sftp = Some(crate::ui::tabs::LinkedSftp::new(view));
        }
        let view = tab.linked_sftp.as_mut()?.view.as_mut();
        view.set_linked_terminal(Some(terminal));
        Some(view)
    }

    fn open_terminal_folder(&mut self, uid: u64, path: String, ctx: &egui::Context) {
        if let Some(view) = self.ensure_linked_sftp(uid, ctx) {
            view.browse_path(path);
            ctx.request_repaint();
        }
    }

    /// Opens files requested from a terminal (`nano <file>`) in the built-in editor.
    fn open_terminal_files(&mut self, uid: u64, files: Vec<String>, ctx: &egui::Context) {
        // Local shells edit files with their own tools: only remote ones go through SFTP.
        let Some((profile, handle)) = self.all_tabs().find_map(|t| match &t.body {
            TabBody::Terminal(term) if t.uid == uid && !t.is_local() => Some((t.profile.clone(), term.handle.clone())),
            _ => None,
        }) else {
            return;
        };
        let profile = self.store.get(profile.id).cloned().unwrap_or(profile);
        let (pool, toasts, ctx) = (self.pool.clone(), self.toasts.clone(), ctx.clone());
        let osc_cwd = handle.shared.lock().ok().and_then(|s| s.parser.callbacks().cwd.clone());
        let (age_secs, rows, cols) = handle.probe_facts();
        self.rt.spawn(async move {
            let conn = match pool.get(&profile, secret::get(profile.secret_id())).await {
                Ok(c) => c,
                Err(e) => {
                    toasts.error(format!("Could not open file in editor: {e:#}"));
                    ctx.request_repaint();
                    return;
                }
            };
            let sftp = match crate::ssh::sftp::Sftp::open(&conn).await {
                Ok(s) => s,
                Err(e) => {
                    toasts.error(format!("Could not open SFTP for editor: {e:#}"));
                    ctx.request_repaint();
                    return;
                }
            };
            let home = sftp.canonicalize(".").await.unwrap_or_else(|_| "/".into());
            let cwd = match osc_cwd {
                Some(c) => c,
                None => {
                    let probe = crate::ssh::cwd::Probe { tag: &handle.tag, age_secs, rows, cols };
                    crate::ssh::cwd::terminal_cwd(&conn, probe).await.unwrap_or_else(|_| home.clone())
                }
            };
            for file in files {
                let raw_path = match crate::ssh::cwd::resolve_path(&conn, &sftp, &file, &cwd).await {
                    Ok(path) => path,
                    Err(e) => {
                        toasts.error(format!("Could not resolve remote path: {e:#}"));
                        continue;
                    }
                };
                let remote_path = crate::ui::path_complete::normalize_remote(&raw_path);
                let read = match crate::remote_edit::read_capped(&sftp, &remote_path, crate::remote_edit::MAX_EDITOR_BYTES).await {
                    Ok(bytes) => Ok(bytes),
                    Err(e) if is_no_such_file(&e) => Ok(Vec::new()),
                    Err(e) => Err(e),
                };
                match read {
                    Ok(bytes) => match crate::remote_edit::decode(&bytes) {
                        Ok(_) => {
                            let pipeline_kb = profile.pipeline_kb;
                            crate::commands::push(crate::commands::AppCommand::OpenEditor(Box::new(crate::commands::EditorDoc {
                                profile: profile.clone(),
                                remote_path,
                                bytes,
                                sftp: sftp.clone(),
                                pipeline_kb,
                            })));
                        }
                        Err(r) => toasts.error(format!("Cannot open {remote_path}: {r}")),
                    },
                    Err(e) => toasts.error(format!("Cannot open {remote_path}: {e:#}")),
                }
            }
            ctx.request_repaint();
        });
    }

    fn modals(&mut self, ctx: &egui::Context) {
        self.custom_boot_modal(ctx);
        self.editor_modal(ctx);
        self.key_manager_modal(ctx);
        self.import_modal(ctx);
        self.prompt_modal(ctx);
        self.delete_modal(ctx);
    }

    fn import_modal(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.import else { return };
        match dialog.show(ctx, &self.store) {
            ImportOutcome::Open => {}
            ImportOutcome::Cancel => self.import = None,
            ImportOutcome::Import(profiles) => {
                let n = profiles.len();
                for p in profiles {
                    self.store.upsert(p);
                }
                // Importers tag hosts with a group ("Bitvise", "SSH config"): file them.
                if self.folders.migrate_groups(&mut self.store.profiles) {
                    let _ = self.folders.save();
                }
                match self.store.save() {
                    Ok(()) => self.toasts.success(format!("Imported {n} host{}", if n == 1 { "" } else { "s" })),
                    Err(e) => self.toasts.error(format!("Could not save profiles: {e:#}")),
                }
                self.import = None;
            }
        }
    }

    fn prompt_modal(&mut self, ctx: &egui::Context) {
        let Some(prompt) = &mut self.prompt else { return };
        let (mut submit, mut cancel) = (false, false);
        let heading = match &prompt.profile.auth {
            AuthMethod::Managed { key } => {
                let name = self.keys.get(*key).map(|k| k.name.clone()).unwrap_or_else(|| "key".into());
                format!("Passphrase for “{name}”")
            }
            _ => format!("Password for {}", prompt.profile.address()),
        };
        let modal = egui::Modal::new(egui::Id::new("pw_prompt")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.heading(heading);
            ui.add_space(8.0);
            let r = ui.add(egui::TextEdit::singleline(&mut prompt.secret).password(true).desired_width(f32::INFINITY));
            r.request_focus();
            submit |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            ui.checkbox(&mut prompt.remember, format!("Remember in {}", crate::platform::credential_store()));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                submit |= ui.add(egui::Button::new("Connect").fill(theme::accent_soft())).clicked();
                cancel |= ui.button("Cancel").clicked();
            });
        });
        if modal.should_close() || cancel {
            self.prompt = None;
            return;
        }
        if !submit {
            return;
        }
        let Some(p) = self.prompt.take() else { return };
        if p.remember
            && let Err(e) = secret::set(p.profile.secret_id(), &p.secret)
        {
            self.toasts.error(format!("{e:#}"));
        }
        match p.action {
            PromptAction::Open(kind) => self.open_now(p.profile, kind, Some(p.secret), ctx),
            PromptAction::Retry(uid) => self.reconnect(uid, Some(p.secret), ctx),
        }
    }

    fn delete_modal(&mut self, ctx: &egui::Context) {
        let Some(id) = self.confirm_delete else { return };
        let name = self.store.get(id).map(|p| p.display_name().to_string()).unwrap_or_default();
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("del_host")).show(ctx, |ui| {
            ui.set_width(300.0);
            ui.heading(format!("Delete “{name}”?"));
            ui.label(RichText::new("The saved password is removed too. Archive it instead to keep it out of the way.").color(theme::text_dim()));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("Delete").color(egui::Color32::WHITE)).fill(theme::danger().gamma_multiply(0.7))).clicked() {
                    self.store.remove(id);
                    let _ = self.store.save();
                    close = true;
                }
                close |= ui.button("Cancel").clicked();
            });
        });
        if close || modal.should_close() {
            self.confirm_delete = None;
        }
    }
}

impl eframe::App for TetherApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Panels cover the window; transparent so window opacity can reach the desktop.
        [0.0; 4]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        crate::themes::follow_system(&ctx);
        self.handle_shortcuts(&ctx);
        self.top_bar(ui);
        self.agents_frame(&ctx);
        self.view_containers(ui);
        {
            let mut docks: Vec<&mut DockState<Tab>> = std::iter::once(&mut self.dock).chain(self.detached.docks_mut()).collect();
            link_terminals(&mut docks, self.focused_terminal);
        }
        self.workspace_area(ui);
        self.finish_view_drag(&ctx);
        self.detached_windows(&ctx);
        self.agent_popover(&ctx);
        self.modals(&ctx);
        self.rename_modal(&ctx);
        self.palette_frame(&ctx);
        if let Some(view) = &mut self.settings_view
            && !view.show(&ctx)
        {
            self.settings_view = None;
        }
        self.track_layout();
        self.toasts.show(&ctx);
        if self.broadcast {
            broadcast_frame(&ctx);
        }
    }

    /// Ends every shell (and what it started) instead of leaving them orphaned.
    fn on_exit(&mut self) {
        if let Some(specs) = self.layout_specs() {
            let _ = self.workspaces.put(LAST_SESSION.into(), Some(specs));
        }
        for tab in self.all_tabs() {
            if let TabBody::Terminal(t) = &tab.body {
                t.handle.close();
            }
        }
    }
}

fn send_initial_remote_cd(view: &mut TerminalView, dir: &str) {
    let trimmed = dir.trim();
    if trimmed.is_empty() || trimmed == "~" {
        return;
    }
    if let Ok(mut s) = view.handle.shared.lock() {
        s.parser.callbacks_mut().cwd = Some(trimmed.to_string());
    }
    let quoted = format!("'{}'", trimmed.replace('\'', "'\\''"));
    view.handle.send(format!("cd -- {quoted}\n").into_bytes());
}

/// A terminal tab as seen by [`link_terminals`].
struct TermRef {
    uid: u64,
    dock: usize,
    leaf: egui_dock::NodePath,
    /// The active tab of its pane (visible).
    visible: bool,
    profile: Uuid,
    handle: TerminalHandle,
}

/// Gives each SFTP pane a terminal of the same host, which enables its "go to terminal
/// folder" button: the last focused one, else one visible in another pane (or window),
/// else any terminal of that host.
fn link_terminals(docks: &mut [&mut DockState<Tab>], focused: Option<u64>) {
    let mut terms = Vec::new();
    for (d, dock) in docks.iter().enumerate() {
        for (path, leaf) in dock.iter_leaves() {
            for (i, tab) in leaf.tabs.iter().enumerate() {
                if let TabBody::Terminal(t) = &tab.body
                    && !tab.is_local()
                {
                    terms.push(TermRef { uid: tab.uid, dock: d, leaf: path, visible: i == leaf.active.0, profile: tab.profile.id, handle: t.handle.clone() });
                }
            }
        }
    }
    for (d, dock) in docks.iter_mut().enumerate() {
        for (path, leaf) in dock.iter_leaves_mut() {
            for tab in leaf.tabs.iter_mut() {
                match &mut tab.body {
                    TabBody::Terminal(term) => {
                        if let Some(linked_sftp) = &mut tab.linked_sftp {
                            linked_sftp.view.set_linked_terminal(Some(term.handle.clone()));
                        }
                    }
                    TabBody::Sftp(view) => {
                        let same_host = |t: &&TermRef| t.profile == tab.profile.id;
                        let elsewhere = |t: &&TermRef| t.dock != d || t.leaf != path;
                        let linked = terms
                            .iter()
                            .filter(same_host)
                            .max_by_key(|t| (Some(t.uid) == focused && t.visible, t.visible && elsewhere(t), elsewhere(t)))
                            .map(|t| t.handle.clone());
                        view.set_linked_terminal(linked);
                    }
                    TabBody::Editor(_) => {}
                }
            }
        }
    }
}

/// Red hairline around the window while broadcast mode is on.
fn broadcast_frame(ctx: &egui::Context) {
    let rect = ctx.content_rect().shrink(1.0);
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("bc_frame")));
    let pulse = ((ctx.input(|i| i.time) * 3.0).sin() * 0.5 + 0.5) as f32;
    painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.5, theme::danger().gamma_multiply(0.4 + 0.4 * pulse)), egui::StrokeKind::Inside);
    ctx.request_repaint_after(std::time::Duration::from_millis(33));
}

fn is_no_such_file(err: &anyhow::Error) -> bool {
    err.chain().any(|c| {
        matches!(
            c.downcast_ref::<russh_sftp::client::error::Error>(),
            Some(russh_sftp::client::error::Error::Status(s)) if s.status_code == russh_sftp::protocol::StatusCode::NoSuchFile
        )
    })
}
