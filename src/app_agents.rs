//! App glue for AI agent sessions: feeding the hub every frame, notifications, the
//! agents panel, jumping to sessions and the "+" menu launchers.
//! A child module of `app` so it can use the app's private state.

use std::sync::Mutex;
use std::time::Instant;

use eframe::egui::{self, RichText};
use egui_dock::DockState;

use super::TetherApp;
use crate::agents::hub::{Alert, TermInfo};
use crate::agents::{AgentKind, AgentState, install, notify};
use crate::keybindings::{self, Action};
use crate::layout::View;
use crate::theme;
use crate::ui::agent_status::PopoverAction;
use crate::ui::agents_panel::{LaunchPick, PanelAction};
use crate::ui::tabs::{Tab, TabBody};

/// Local terminals of `dock`, with whether each is the visible tab of its pane.
fn local_terms<'a>(dock: &'a DockState<Tab>, out: &mut Vec<TermInfo<'a>>) {
    for (_, leaf) in dock.iter_leaves() {
        for (i, tab) in leaf.tabs.iter().enumerate() {
            if let (TabBody::Terminal(view), true) = (&tab.body, tab.is_local()) {
                out.push(TermInfo {
                    uid: tab.uid,
                    handle: &view.handle,
                    visible: i == leaf.active.0,
                    label: tab.profile.display_name(),
                    custom: tab.custom_title.as_deref(),
                });
            }
        }
    }
}

/// Whether the agent's hooks are in its config, read once per opened popover.
fn hooks_installed(uid: u64, kind: AgentKind) -> bool {
    static CACHE: Mutex<Option<(u64, bool)>> = Mutex::new(None);
    let Ok(mut cache) = CACHE.lock() else { return false };
    if let Some((u, installed)) = *cache
        && u == uid
    {
        return installed;
    }
    let target = match kind {
        AgentKind::Claude => Some(install::Target::Claude),
        AgentKind::Codex => Some(install::Target::Codex),
        _ => None,
    };
    let installed = target.is_some_and(|t| install::status(t) == install::Status::Installed);
    *cache = Some((uid, installed));
    installed
}

/// Activates tab `uid` in `dock`; true when found.
fn activate(dock: &mut DockState<Tab>, uid: u64) -> bool {
    let Some(path) = dock.find_tab_from(|t| t.uid == uid) else { return false };
    let _ = dock.set_active_tab(path);
    dock.set_focused_node_and_surface(path.node_path());
    if let Some((_, Tab { body: TabBody::Terminal(view), .. })) = dock.iter_all_tabs_mut().find(|(_, t)| t.uid == uid) {
        view.want_focus = true;
    }
    true
}

impl TetherApp {
    pub(super) fn agents_frame(&mut self, ctx: &egui::Context) {
        let settings = crate::settings::snapshot();
        let window_focused = ctx.input(|i| i.focused);
        let monitor = self.agents.monitor();
        monitor.set_focused(window_focused);
        monitor.set_interval(settings.agents.monitor_interval());
        let toasts = self.toasts.clone();
        let mut terms = Vec::new();
        local_terms(&self.dock, &mut terms);
        for dock in self.detached.docks_mut() {
            local_terms(dock, &mut terms);
        }
        let mut visited = Vec::new();
        self.project_cwds.retain(|uid, _| terms.iter().any(|t| t.uid == *uid));
        for term in &terms {
            let cwd = term.handle.shared.lock().ok().and_then(|s| s.parser.callbacks().cwd.clone());
            if let Some(cwd) = cwd.filter(|s| !s.is_empty() && s.len() <= 4096)
                && self.project_cwds.get(&term.uid) != Some(&cwd)
            {
                self.project_cwds.insert(term.uid, cwd.clone());
                visited.push(cwd);
            }
        }
        if !visited.is_empty() {
            let _ = crate::settings::update(|s| {
                for path in visited {
                    s.projects.record(&path, crate::local_projects::now());
                }
            });
        }
        Self::command_notices(&toasts, ctx, &terms, window_focused);
        let alerts = self.agents.frame(ctx, terms, Instant::now());
        for alert in alerts {
            self.alert(ctx, alert, window_focused);
        }
    }

    fn alert(&self, ctx: &egui::Context, a: Alert, window_focused: bool) {
        let cfg = &crate::settings::snapshot().agents;
        if a.state == AgentState::Done && !cfg.notify_on_done {
            return;
        }
        let title = format!("{} · {}", a.kind.name(), a.state.label());
        let body = match &a.message {
            // Text a program printed could pose as anything: say where it came from.
            Some(m) if a.from_terminal => format!("{}\nTerminal: {}", a.project, crate::agents::snippet(m, 160)),
            Some(m) => format!("{}\n{}", a.project, crate::agents::snippet(m, 160)),
            None => a.project.clone(),
        };
        if !window_focused {
            if cfg.desktop_notifications {
                notify::desktop(title, body);
            }
            if cfg.flash_taskbar {
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
            }
        } else if !a.visible {
            let hint = keybindings::label(Action::NextAttention);
            let jump = if hint.is_empty() { String::new() } else { format!("  ({hint} to jump)") };
            self.toasts.info(format!("{title}: {}{jump}", a.project));
        }
    }

    /// Shell integration: a long command finished in a hidden tab or while tether was in
    /// the background. Same channels as agent alerts (toast, or desktop + taskbar flash).
    fn command_notices(toasts: &crate::ui::toast::Toasts, ctx: &egui::Context, terms: &[TermInfo], window_focused: bool) {
        let settings = crate::settings::snapshot();
        for t in terms {
            // Busy terminals are looked at next frame (`take_finished` keeps the news).
            let Some(done) = t.handle.shared.try_lock().ok().and_then(|mut s| s.parser.callbacks_mut().shell.take_finished()) else { continue };
            if t.visible && window_focused {
                continue;
            }
            let Some((title, body)) = crate::ui::blocks::long_command_notice(&done, &settings.shell_integration) else { continue };
            if window_focused {
                toasts.info(format!("{title} in {}: {body}", t.label));
                continue;
            }
            if settings.agents.desktop_notifications {
                notify::desktop(format!("{title} · {}", t.label), body);
            }
            if settings.agents.flash_taskbar {
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
            }
        }
    }

    pub(super) fn note_agent_input(&mut self, uid: u64) {
        self.agents.note_input(uid, Instant::now());
    }

    /// Shows tab `uid` wherever it lives and gives it the keyboard.
    pub(super) fn focus_tab(&mut self, uid: u64, ctx: &egui::Context) {
        if activate(&mut self.dock, uid) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            return;
        }
        for dock in self.detached.docks_mut() {
            if activate(dock, uid) {
                return;
            }
        }
    }

    /// Ctrl+Shift+A: the next session waiting for the user (most urgent first).
    pub(super) fn focus_next_attention(&mut self, ctx: &egui::Context) {
        let current = self.focused_terminal.or_else(|| self.dock.find_active_focused().map(|(_, t)| t.uid));
        match self.agents.next_attention(current) {
            Some(uid) => self.focus_tab(uid, ctx),
            None => self.toasts.info("No agent session needs attention"),
        }
    }

    /// Top-bar entry, only while sessions need the user: a warm badge with the count
    /// that jumps to the most urgent one.
    pub(super) fn agents_top_bar(&mut self, ui: &mut egui::Ui) {
        let waiting = self.agents.attention_count();
        if waiting == 0 {
            return;
        }
        let text = RichText::new(format!("{waiting}")).size(12.0).strong().color(theme::warn());
        let tip = keybindings::hint(
            &format!("{waiting} agent session{} need{} you: jump to the next", if waiting == 1 { "" } else { "s" }, if waiting == 1 { "s" } else { "" }),
            Action::NextAttention,
        );
        let resp = super::chrome::chrome_button(ui, Some(crate::ui::settings_widgets::Icon::Sparkle), Some(text), super::chrome::ButtonTone::Warn, &tip);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Agents · {waiting} need attention")));
        if resp.clicked() {
            self.focus_next_attention(ui.ctx());
        }
    }

    /// The status pill's popover (opened from a tab, a row or a pane), on top of everything.
    pub(super) fn agent_popover(&mut self, ctx: &egui::Context) {
        let Some(uid) = crate::ui::agent_status::popover_uid() else { return };
        let info = self.agents.sessions().iter().find(|s| s.uid == uid).cloned();
        let hooks_installed = info.as_ref().is_some_and(|s| hooks_installed(uid, s.kind));
        let data = info.as_ref().map(|info| crate::ui::agent_status::PopoverData { info, title: &info.title, hooks_installed, now: Instant::now() });
        match crate::ui::agent_status::show_popover(ctx, data) {
            Some(PopoverAction::Jump(uid)) => self.focus_tab(uid, ctx),
            Some(PopoverAction::MarkRead(uid)) => self.agents.mark_read(uid),
            Some(PopoverAction::InstallHooks) => self.apply_agents_panel(Some(PanelAction::InstallHooks), ctx),
            None => {}
        }
    }

    /// Ctrl+Shift+J / the top-bar button: show the "Agents" view (wherever it lives,
    /// opening its container), or fold that container when it is already showing.
    pub(super) fn toggle_agents(&mut self) {
        let _ = crate::settings::update(|s| s.layout.views.click(View::Agents));
    }

    pub(super) fn apply_agents_panel(&mut self, action: Option<PanelAction>, ctx: &egui::Context) {
        match action {
            Some(PanelAction::Focus(uid)) => self.focus_tab(uid, ctx),
            Some(PanelAction::Close) => self.toggle_agents(),
            Some(PanelAction::CustomBoot(kind)) => self.custom_boot = Some(crate::ui::agents_panel::CustomBoot::new(kind)),
            Some(PanelAction::Launch(pick)) => {
                let target = self.dock.focused_leaf().or_else(|| self.dock.iter_leaves().next().map(|(p, _)| p));
                let target = target.unwrap_or(egui_dock::NodePath { surface: egui_dock::SurfaceIndex::main(), node: egui_dock::NodeIndex::root() });
                self.open_agents_in(vec![(target, pick)], ctx);
            }
            Some(PanelAction::InstallHooks) => {
                self.settings_view = Some(crate::ui::settings_view::SettingsView::at(crate::ui::settings_view::Section::Agents));
            }
            None => {}
        }
    }

    pub(super) fn launch_agent_command(&mut self, kind: AgentKind, command: String, ctx: &egui::Context) {
        self.apply_agents_panel(Some(PanelAction::Launch(LaunchPick { kind, command, shell_id: None, cwd: None })), ctx);
    }

    pub(super) fn custom_boot_modal(&mut self, ctx: &egui::Context) {
        let Some(boot) = &mut self.custom_boot else { return };
        let (command, close) = boot.show(ctx);
        let target = boot.target;
        if close {
            self.custom_boot = None;
        }
        if let Some(command) = command {
            if let Some(target) = target {
                self.open_agents_in(vec![(target, command)], ctx);
            } else {
                self.apply_agents_panel(Some(PanelAction::Launch(command)), ctx);
            }
        }
    }

    /// Opens agents picked from panes' "+" menus: the default shell, then the command.
    pub(super) fn open_agents_in(&mut self, picks: Vec<(egui_dock::NodePath, LaunchPick)>, ctx: &egui::Context) {
        for (path, pick) in picks {
            if pick.cwd.is_none() {
                self.custom_boot = Some(crate::ui::agents_panel::CustomBoot::launch(pick, path));
                continue;
            }
            let prefs = crate::settings::snapshot();
            let shell_id = pick.shell_id.as_deref().or_else(|| prefs.agents.launch_shells.get(pick.kind.id()).map(String::as_str));
            let Some(shell) = shell_id.and_then(crate::pty::shells::find).or_else(crate::pty::shells::default_shell) else {
                self.toasts.error("No local shell was found on this machine");
                return;
            };
            let cwd = pick.cwd;
            if let Some(dir) = &cwd {
                if !dir.is_dir() {
                    self.toasts.error("The project folder no longer exists");
                    continue;
                }
                let _ = crate::settings::update(|s| s.projects.record(&dir.display().to_string(), crate::local_projects::now()));
            }
            if pick.shell_id.is_some() {
                let _ = crate::settings::update(|s| {
                    s.agents.launch_shells.insert(pick.kind.id().into(), shell.id.clone());
                });
            }
            let tab = self.local_tab(shell, cwd, ctx);
            if let TabBody::Terminal(view) = &tab.body {
                // The pty buffers this until the shell reads its first line.
                view.handle.send(format!("{}\r", pick.command).into_bytes());
            }
            match self.dock.leaf_mut(path) {
                Ok(leaf) => {
                    leaf.append_tab(tab);
                    self.dock.set_focused_node_and_surface(path);
                }
                Err(_) => self.dock.push_to_focused_leaf(tab),
            }
        }
    }
}
