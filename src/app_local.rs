//! Opening local shells (`pty`): new tabs, the pane "+" menu, respawning on reconnect.
//! A child module of `app` so it can use the app's private state.

use std::path::PathBuf;

use eframe::egui;

use super::TetherApp;
use crate::pty::{self, ShellSpec};
use crate::ssh::terminal::{LinkState, TerminalHandle};
use crate::ui::tabs::{Tab, TabBody};
use crate::ui::terminal_view::TerminalView;

impl TetherApp {
    /// Starts `spec` on its own thread (the pty read/write loops block).
    pub(super) fn spawn_local(&self, spec: &ShellSpec, cwd: Option<PathBuf>, ctx: &egui::Context) -> TerminalView {
        let initial_cwd = cwd.as_ref().map(|p| p.to_string_lossy().into_owned());
        let (handle, rx) = TerminalHandle::new(24, 80);
        // Wakes the UI even if this tab is never drawn (restored workspace, background tab).
        handle.repaint.attach(ctx);
        let (spec_owned, h, ctx) = (spec.clone(), handle.clone(), ctx.clone());
        let started = std::thread::Builder::new().name("tether-pty".into()).spawn(move || {
            let repaint = {
                let ctx = ctx.clone();
                move |after| ctx.request_repaint_after(after)
            };
            if let Err(e) = pty::run_local_pty(&spec_owned, cwd, h.clone(), rx, repaint) {
                h.set_state(LinkState::Failed(format!("{e:#}")));
            }
            ctx.request_repaint();
        });
        if let Err(e) = started {
            handle.set_state(LinkState::Failed(format!("Could not start a thread for the shell: {e}")));
        }
        let mut view = TerminalView::new(handle);
        view.intercept_nano = false;
        view.fallback_cwd = initial_cwd;
        view
    }

    pub(super) fn local_tab(&self, spec: &ShellSpec, cwd: Option<PathBuf>, ctx: &egui::Context) -> Tab {
        Tab::new_local(spec.clone(), self.spawn_local(spec, cwd, ctx))
    }

    /// Opens `spec` (or the active local shell / default shell) as a full-pane session
    /// inserted right below the currently active session in the tab order, inheriting the
    /// active terminal's working directory (`cwd`).
    pub(super) fn open_local(&mut self, spec: Option<&'static ShellSpec>, ctx: &egui::Context) {
        let active_origin = self.dock.find_active_focused().and_then(|(_, active)| origin(active));
        if let Some((active_spec, active_cwd)) = active_origin {
            let chosen = spec.cloned().unwrap_or(active_spec);
            let tab = self.local_tab(&chosen, active_cwd, ctx);
            self.insert_after_active(tab);
            return;
        }
        let Some(spec) = spec.or_else(pty::shells::default_shell) else {
            self.toasts.error("No local shell was found on this machine");
            return;
        };
        let tab = self.local_tab(spec, None, ctx);
        self.insert_after_active(tab);
    }

    /// Inserts `tab` into the focused pane immediately after the currently active tab
    /// (so in the Sessions list it sits right below the current session even when more
    /// sessions follow) and switches the full pane to it.
    pub(super) fn insert_after_active(&mut self, tab: Tab) {
        let new_uid = tab.uid;
        if let Some(leaf_path) = self.dock.focused_leaf()
            && let Ok(leaf) = self.dock.leaf_mut(leaf_path)
        {
            let idx = (leaf.active.0 + 1).min(leaf.tabs.len());
            leaf.insert_tab(egui_dock::TabIndex(idx), tab);
            self.show_tab(new_uid);
            return;
        }
        self.dock.push_to_focused_leaf(tab);
        self.show_tab(new_uid);
    }

    /// Opens shells picked from panes' "+" menus in those panes, inheriting the active
    /// local terminal's working directory when available and placing the new tab right
    /// after the active one.
    pub(super) fn open_local_in(&mut self, picks: Vec<(egui_dock::NodePath, &'static ShellSpec)>, ctx: &egui::Context) {
        for (path, spec) in picks {
            let inherited_cwd = self.dock.leaf_mut(path).ok().and_then(|leaf| leaf.tabs.get(leaf.active.0)).and_then(origin).and_then(|(_, cwd)| cwd);
            let tab = self.local_tab(spec, inherited_cwd, ctx);
            let new_uid = tab.uid;
            match self.dock.leaf_mut(path) {
                Ok(leaf) => {
                    let idx = (leaf.active.0 + 1).min(leaf.tabs.len());
                    leaf.insert_tab(egui_dock::TabIndex(idx), tab);
                    self.show_tab(new_uid);
                }
                Err(_) => self.insert_after_active(tab),
            }
        }
    }

    /// Respawns a local tab's shell in place (its "Reconnect" button), keeping its path.
    pub(super) fn respawn_local(&mut self, uid: u64, ctx: &egui::Context) {
        let Some((spec, cwd)) = self.all_tabs().find(|t| t.uid == uid).and_then(origin) else { return };
        let view = self.spawn_local(&spec, cwd, ctx);
        let tab = self.dock.iter_all_tabs_mut().map(|(_, t)| t).chain(self.detached.tabs_mut()).chain(self.mini_dock.tabs_mut()).find(|t| t.uid == uid);
        if let Some(Tab { body: TabBody::Terminal(old), .. }) = tab {
            **old = view;
        }
    }
}

/// The shell a local `tab` runs and the folder it is currently in (AI agent hook ->
/// OSC 7 -> live OS process CWD -> fallback CWD), to start another one like it.
pub(super) fn origin(tab: &Tab) -> Option<(ShellSpec, Option<PathBuf>)> {
    let spec = tab.local.clone()?;
    let cwd = match &tab.body {
        TabBody::Terminal(t) => {
            let (osc_cwd, pid) = t.handle.shared.lock().ok().map(|s| (s.parser.callbacks().cwd.clone(), s.pid)).unwrap_or((None, None));
            crate::agents::hub::agent_cwd(tab.uid).or(osc_cwd).or_else(|| pid.and_then(live_pid_cwd)).or_else(|| tab.current_cwd()).map(PathBuf::from)
        }
        _ => None,
    };
    Some((spec, cwd))
}

fn live_pid_cwd(pid: u32) -> Option<String> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let target = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[target]), true, ProcessRefreshKind::nothing().with_cwd(UpdateKind::Always));
    sys.process(target)?.cwd().map(|p| p.to_string_lossy().into_owned()).filter(|s| !s.is_empty())
}
