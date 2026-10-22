//! What the command palette lists and how the app runs a chosen entry.
//! A child module of `app`. New built-in entries go in `build_registry`; other features
//! can add theirs with `actions::register_provider`.

use eframe::egui;

use super::TetherApp;
use crate::actions::{Command, Entry, Registry};
use crate::keybindings::{Action, Group};
use crate::layout::{BarOrientation, Container, Direction, TabPosition, View};
use crate::settings;
use crate::ui::palette::{Palette, PaletteOutcome};
use crate::ui::settings_view::SettingsView;
use crate::ui::sidebar::OpenKind;
use crate::ui::tabs::{Tab, TabBody};

impl TetherApp {
    pub(super) fn open_palette(&mut self) {
        if self.palette.is_none() {
            self.palette = Some(Palette::new(self.build_registry()));
        }
    }

    fn build_registry(&mut self) -> Registry {
        let mut r = Registry::default();
        let l = settings::snapshot().layout.clone();
        // Open tabs first: "go to" is the most common use.
        for (path, tab) in self.dock.iter_all_tabs_mut() {
            if !path.surface.is_main() {
                continue;
            }
            let title = tab.display_title();
            r.add(Entry::new("Tab", title, Command::FocusTab(tab.uid)).detail(tab.subtitle()));
        }
        // Not waiting on detection: until it is done the palette just lists no shells.
        let shells = crate::pty::shells::detected().unwrap_or_default();
        let default = crate::pty::shells::pick_default(shells, &settings::get().terminal.default_shell).map(|s| s.id.as_str());
        for shell in shells {
            let keys = (default == Some(shell.id.as_str())).then(|| crate::keybindings::label(Action::NewLocalTerminal));
            r.add(Entry::new("Shell", format!("New {}", shell.name), Command::NewShell(shell.id.clone())).keys(keys));
        }
        let mut hosts: Vec<_> = self.store.profiles.iter().filter(|p| !p.archived).collect();
        hosts.sort_by_key(|p| std::cmp::Reverse(p.last_used));
        for p in &hosts {
            r.add(Entry::new("Host", format!("Connect to {}", p.display_name()), Command::Connect(p.id)).detail(p.address()));
        }
        for a in Action::ALL {
            // The "Focus … view" entries below cover these.
            let view_focus = matches!(a, Action::FocusSessionsView | Action::FocusWorkspacesView);
            if (is_global(a) || matches!(a, Action::PrevPrompt | Action::NextPrompt)) && !view_focus {
                r.add(Entry::action(category(a.group()), a));
            }
        }
        for v in View::ALL {
            let keys = crate::keybindings::binding(crate::ui::activity::view_action(v)).map(|b| b.label());
            r.add(Entry::new("View", format!("Focus {} view", v.title()), Command::ShowView(v)).keys(keys));
        }
        for v in View::ALL {
            let here = l.views.container_of(v);
            for c in Container::ALL.into_iter().filter(|c| Some(*c) != here) {
                r.add(Entry::new("View", format!("Move {} view to {}", v.title(), c.title().to_lowercase()), Command::MoveView(v, c)));
            }
        }
        for p in TabPosition::ALL.into_iter().filter(|p| *p != l.tab_position) {
            let title = match p {
                TabPosition::Top => "Tabs on top of panes",
                TabPosition::Sidebar => "Vertical tabs in the Sessions view",
            };
            r.add(Entry::new("Layout", title, Command::SetTabPosition(p)));
        }
        let other = l.sidebar_side.flip();
        r.add(Entry::new("Layout", format!("Primary side bar on the {}", other.label().to_lowercase()), Command::SetSidebarSide(other)));
        for c in [Container::Primary, Container::Secondary] {
            let bar = match l.views.get(c).bar {
                BarOrientation::Side => BarOrientation::Top,
                BarOrientation::Top => BarOrientation::Side,
            };
            let place = if bar == BarOrientation::Top { "on top" } else { "on the side" };
            r.add(Entry::new("Layout", format!("{} icons {place}", c.title()), Command::SetBar(c, bar)));
        }
        let toggle = |on: bool| if on { "off" } else { "on" };
        r.add(Entry::new("Layout", format!("Turn {} dim inactive panes", toggle(l.dim_inactive_panes)), Command::ToggleDimPanes));
        r.add(Entry::new("Layout", format!("Turn {} focus follows mouse", toggle(l.focus_follows_mouse)), Command::ToggleFocusFollowsMouse));
        r.add(Entry::new("Layout", "Layout settings…", Command::OpenLayoutSettings));
        r.add(Entry::new("Hosts", "Import hosts…", Command::ImportHosts).detail("Bitvise profiles, SSH config"));
        for l in crate::agents::launch::available() {
            let (kind, command) = (l.kind, l.command);
            r.add(
                Entry::new("Agent", format!("Launch {}…", kind.name()), Command::LaunchAgent { kind, command }).detail("Choose a project directory and shell"),
            );
        }
        for name in self.workspaces.names() {
            r.add(Entry::new("Workspace", format!("Restore {name}"), Command::RestoreWorkspace(name)));
        }
        for p in &hosts {
            r.add(Entry::new("SFTP", format!("Browse files on {}", p.display_name()), Command::ConnectSftp(p.id)).detail(p.address()));
        }
        let current = settings::snapshot().appearance.theme.clone();
        for t in crate::themes::all().iter().filter(|t| !t.name.eq_ignore_ascii_case(&current)) {
            r.add(Entry::new("Theme", t.name.clone(), Command::SetTheme(t.name.clone())).detail(t.tagline.clone()));
        }
        r.add_providers();
        r
    }

    pub(super) fn palette_frame(&mut self, ctx: &egui::Context) {
        let Some(palette) = &mut self.palette else { return };
        match palette.show(ctx) {
            PaletteOutcome::Open => {}
            PaletteOutcome::Close => self.palette = None,
            PaletteOutcome::Run(cmd) => {
                self.palette = None;
                self.run_command(cmd, ctx);
            }
        }
    }

    fn run_command(&mut self, cmd: Command, ctx: &egui::Context) {
        match cmd {
            Command::App(a) => self.run_action(a, ctx),
            Command::NewShell(id) => self.open_local(crate::pty::shells::find(&id), ctx),
            Command::Connect(id) => {
                if let Some(p) = self.store.get(id).cloned() {
                    self.request_open(p, OpenKind::Terminal, ctx);
                }
            }
            Command::ConnectSftp(id) => {
                if let Some(p) = self.store.get(id).cloned() {
                    self.request_open(p, OpenKind::Sftp, ctx);
                }
            }
            Command::SetTheme(name) => {
                let _ = settings::update(|s| {
                    s.appearance.theme = name.clone();
                    s.appearance.sync_with_os = false;
                });
                crate::themes::apply_animated(ctx, &name);
            }
            Command::SetTabPosition(p) => super::layout_ui::set_tab_position(p),
            Command::SetSidebarSide(side) => super::layout_ui::set_sidebar_side(side),
            Command::ShowView(v) => super::layout_ui::reveal_view(v),
            Command::MoveView(v, c) => {
                let _ = settings::update(|s| s.layout.views.move_view(v, c, None));
            }
            Command::SetBar(c, bar) => {
                let _ = settings::update(|s| s.layout.views.get_mut(c).bar = bar);
            }
            Command::ToggleDimPanes => {
                let _ = settings::update(|s| s.layout.dim_inactive_panes = !s.layout.dim_inactive_panes);
            }
            Command::ToggleFocusFollowsMouse => {
                let _ = settings::update(|s| s.layout.focus_follows_mouse = !s.layout.focus_follows_mouse);
            }
            Command::FocusTab(uid) => self.show_tab(uid),
            Command::RestoreWorkspace(name) => self.restore_workspace(&name, ctx),
            Command::OpenLayoutSettings => self.open_layout_settings(),
            Command::ImportHosts => self.import = Some(crate::ui::import_view::ImportDialog::new(&self.store)),
            Command::LaunchAgent { kind, command } => {
                let pick = crate::ui::agents_panel::LaunchPick { kind, command: command.into(), shell_id: None, cwd: None };
                let target = self
                    .dock
                    .focused_leaf()
                    .or_else(|| self.dock.iter_leaves().next().map(|(p, _)| p))
                    .unwrap_or(egui_dock::NodePath { surface: egui_dock::SurfaceIndex::main(), node: egui_dock::NodeIndex::root() });
                self.open_agents_in(vec![(target, pick)], ctx);
            }
            Command::Run(f) => f(ctx),
        }
    }

    /// Runs a shortcut action (from its key or the palette).
    pub(super) fn run_action(&mut self, a: Action, ctx: &egui::Context) {
        use crate::workspace::TabKind;
        use egui_dock::Split;
        match a {
            Action::CommandPalette => {
                if self.palette.take().is_none() {
                    self.open_palette();
                }
            }
            Action::ToggleSidebar => super::layout_ui::toggle_container(Container::Primary),
            Action::ToggleSecondarySidebar => super::layout_ui::toggle_container(Container::Secondary),
            Action::TogglePanel => super::layout_ui::toggle_container(Container::Panel),
            Action::FocusSessionsView => super::layout_ui::reveal_view(View::Sessions),
            Action::FocusWorkspacesView => super::layout_ui::reveal_view(View::Workspaces),
            Action::ResetLayout => super::views_ui::reset_layout(ctx),
            Action::OpenSettings => {
                self.settings_view.get_or_insert_with(SettingsView::new);
            }
            Action::KeyManager => {
                if self.key_manager.is_none() {
                    self.key_manager = Some(crate::ui::key_manager::KeyManager::new(&self.keys));
                }
            }
            Action::NewHost => {
                if self.editor.is_none() {
                    self.open_editor_new(None);
                }
            }
            Action::NewFolder => self.new_folder(None),
            Action::QuickConnect => {
                super::layout_ui::reveal_view(View::Hosts);
                self.sidebar.focus_quick_connect();
            }
            Action::SearchHosts => {
                super::layout_ui::reveal_view(View::Hosts);
                self.sidebar.focus_search();
            }
            Action::NewLocalTerminal => self.open_local(None, ctx),
            Action::ToggleBroadcast => self.broadcast = !self.broadcast,
            Action::SplitRight => self.duplicate_focused(Split::Right, TabKind::Terminal, ctx),
            Action::SplitDown => self.duplicate_focused(Split::Below, TabKind::Terminal, ctx),
            Action::OpenSftp => self.open_linked_sftp(ctx),
            Action::CloseTab => self.close_focused(),
            Action::NextTab => self.cycle_tab(true),
            Action::PrevTab => self.cycle_tab(false),
            Action::FocusPaneLeft => self.focus_pane(Direction::Left),
            Action::FocusPaneRight => self.focus_pane(Direction::Right),
            Action::FocusPaneUp => self.focus_pane(Direction::Up),
            Action::FocusPaneDown => self.focus_pane(Direction::Down),
            Action::ZoomPane => self.toggle_zoom(),
            Action::CycleTabPosition => super::layout_ui::set_tab_position(settings::snapshot().layout.tab_position.next()),
            Action::MoveSidebar => super::layout_ui::set_sidebar_side(settings::snapshot().layout.sidebar_side.flip()),
            Action::RenameTab => {
                if let Some(uid) = self.active_uid() {
                    self.apply_tab_actions(vec![crate::ui::vertical_tabs::TabAction::StartRename(uid)], ctx);
                }
            }
            Action::NextAttention => self.focus_next_attention(ctx),
            Action::ToggleAgents => self.toggle_agents(),
            Action::ToggleCodex => {
                let _ = crate::settings::update(|s| s.layout.views.click(View::Codex));
            }
            // Handled by the focused terminal / hovered SFTP pane themselves.
            Action::PrevPrompt | Action::NextPrompt => {
                let up = a == Action::PrevPrompt;
                let stepped = match self.dock.find_active_focused() {
                    Some((_, Tab { body: TabBody::Terminal(view), .. })) => view.step_prompt(up, ctx),
                    _ => false,
                };
                if !stepped {
                    self.toasts.info("No command blocks in this terminal (shell integration is off or not loaded)");
                }
            }
            Action::FontBigger | Action::FontSmaller | Action::FontReset | Action::SftpBack | Action::SftpForward | Action::SftpUp | Action::SftpRefresh => {}
        }
    }
}

fn category(g: Group) -> &'static str {
    match g {
        Group::General => "General",
        Group::Sessions => "Tab",
        Group::Panes => "Pane",
        Group::Terminal => "Terminal",
        Group::Sftp => "SFTP",
    }
}

/// Actions handled globally (others are left to the widget that has focus or hover).
pub(super) fn is_global(a: Action) -> bool {
    !matches!(a, Action::FontBigger | Action::FontSmaller | Action::FontReset | Action::PrevPrompt | Action::NextPrompt) && a.group() != Group::Sftp
}
