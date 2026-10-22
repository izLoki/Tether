//! Settings → AI agents: session tracking, the resource monitor, and installing the
//! hooks that report exact states (Claude Code hooks, Codex `notify`). Every write to an
//! agent's config is shown as a diff first and needs a click to confirm.

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, Ui};

use super::settings_widgets::{self as w, SliderSpec};
use crate::agents::install::{self, Plan, Status, Target};
use crate::settings::{AgentSettings, Settings};
use crate::theme;

/// File status is re-read at most this often while the section is open.
const STATUS_TTL: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct AgentsState {
    plan: Option<Plan>,
    message: Option<(bool, String)>,
    status: Option<(Instant, Status, Status)>,
}

impl AgentsState {
    fn statuses(&mut self) -> (Status, Status) {
        let stale = self.status.as_ref().is_none_or(|(at, _, _)| at.elapsed() > STATUS_TTL);
        if stale {
            self.status = Some((Instant::now(), install::status(Target::Claude), install::status(Target::Codex)));
        }
        self.status.as_ref().map(|(_, a, b)| (a.clone(), b.clone())).unwrap_or((Status::NotInstalled, Status::NotInstalled))
    }
}

pub fn show(ui: &mut Ui, s: &mut Settings, st: &mut AgentsState, gap: f32) {
    let a = &mut s.agents;
    w::card(ui, Some("Sessions"), |ui| {
        w::row(ui, "Status badges", "Working / waiting / done marks on tabs running Claude Code, Codex and other agents", |ui| {
            w::toggle(ui, &mut a.badges);
        });
        w::separator(ui);
        w::row(ui, "Desktop notifications", "When a session needs you while tether is in the background", |ui| {
            w::toggle(ui, &mut a.desktop_notifications);
        });
        w::separator(ui);
        w::row(ui, "Flash the taskbar", "Same moment, on the taskbar button", |ui| {
            w::toggle(ui, &mut a.flash_taskbar);
        });
        w::separator(ui);
        w::row(ui, "Notify when a turn is done", "Not only when the agent asks for input or permission", |ui| {
            w::toggle(ui, &mut a.notify_on_done);
        });
    });
    ui.add_space(gap);
    w::card(ui, Some("Resource monitor"), |ui| {
        w::row(ui, "CPU and memory on tabs", "Sum of each local terminal's process tree, plus the app total in the top bar", |ui| {
            w::toggle(ui, &mut a.show_usage);
        });
        w::separator(ui);
        let mut secs = a.monitor_interval().as_secs_f32();
        w::row(ui, "Sampling interval", "Slows to 5 s while tether is in the background", |ui| {
            let spec = SliderSpec {
                min: AgentSettings::INTERVAL_MIN_MS as f32 / 1000.0,
                max: AgentSettings::INTERVAL_MAX_MS as f32 / 1000.0,
                step: 0.5,
                log: false,
                format: &|v| format!("{v:.1} s"),
            };
            if w::slider_with_value(ui, &mut secs, &spec).changed() {
                a.monitor_interval_ms = (secs * 1000.0).round() as u64;
            }
        });
    });
    ui.add_space(gap);
    w::card(ui, Some("Agent signals"), |ui| hooks(ui, st));
}

fn status_text(status: &Status) -> (String, egui::Color32) {
    match status {
        Status::Installed => ("Installed".into(), theme::success()),
        Status::NotInstalled => ("Not installed: states are guessed from the terminal".into(), theme::text_dim()),
        Status::Conflict(existing) => (format!("Another notify program is set ({existing}); tether will not replace it"), theme::warn()),
        Status::Unreadable(e) => (e.clone(), theme::danger()),
    }
}

fn hooks(ui: &mut Ui, st: &mut AgentsState) {
    ui.label(
        RichText::new(
            "Claude hooks report turn states. Codex notify reports completed turns; terminal activity and process monitoring track ongoing work. \
             These signals only act inside Tether terminals and never block the agent.",
        )
        .size(11.5)
        .color(theme::text_dim()),
    );
    ui.add_space(6.0);
    let (claude, codex) = st.statuses();
    let mut request = None;
    for (target, status, what) in [(Target::Claude, &claude, "~/.claude/settings.json hooks"), (Target::Codex, &codex, "notify in ~/.codex/config.toml")] {
        w::separator(ui);
        let (text, colour) = status_text(status);
        ui.horizontal(|ui| {
            ui.set_min_height(40.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(target.name()).size(13.5).color(theme::text()));
                ui.label(RichText::new(format!("{what} · {text}")).size(11.5).color(colour));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let busy = st.plan.is_some();
                ui.add_enabled_ui(!busy, |ui| {
                    if *status == Status::Installed {
                        if w::ghost_button(ui, "Uninstall", theme::text()).clicked() {
                            request = Some((target, false));
                        }
                    } else if !matches!(status, Status::Conflict(_) | Status::Unreadable(_)) && w::primary_button(ui, "Install…").clicked() {
                        request = Some((target, true));
                    }
                });
            });
        });
    }
    if let Some((target, install)) = request {
        st.message = None;
        match std::env::current_exe().map_err(|e| e.to_string()).and_then(|exe| install::plan(target, install, &exe)) {
            Ok(Some(plan)) => st.plan = Some(plan),
            Ok(None) => st.message = Some((true, format!("{}: nothing to change", target.name()))),
            Err(e) => st.message = Some((false, e)),
        }
    }
    if let Some((ok, text)) = &st.message {
        ui.add_space(6.0);
        ui.label(RichText::new(text).size(11.5).color(if *ok { theme::success() } else { theme::danger() }));
    }
    confirm(ui, st);
}

/// The pending change as a diff, with Write / Cancel.
fn confirm(ui: &mut Ui, st: &mut AgentsState) {
    let Some(plan) = &st.plan else { return };
    ui.add_space(10.0);
    let verb = if plan.install { "Add tether's hooks to" } else { "Remove tether's hooks from" };
    ui.label(RichText::new(format!("{verb} {}", plan.path.display())).size(12.5).strong());
    let backup = if plan.original.is_some() { "The current file is first copied to *.tether-bak." } else { "The file does not exist yet and will be created." };
    ui.label(RichText::new(backup).size(11.5).color(theme::text_dim()));
    egui::Frame::new().fill(theme::term_bg()).corner_radius(8).inner_margin(10).show(ui, |ui| {
        egui::ScrollArea::vertical().max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
            for (kind, line) in &plan.diff {
                let (colour, text) = match kind {
                    '+' => (theme::success(), format!("+ {line}")),
                    '-' => (theme::danger(), format!("- {line}")),
                    '…' => (theme::text_faint(), "  …".to_string()),
                    _ => (theme::text_dim(), format!("  {line}")),
                };
                ui.label(RichText::new(text).monospace().size(11.5).color(colour));
            }
        });
    });
    ui.add_space(6.0);
    let (mut write, mut cancel) = (false, false);
    ui.horizontal(|ui| {
        write = w::primary_button(ui, "Write changes").clicked();
        cancel = w::ghost_button(ui, "Cancel", theme::text()).clicked();
    });
    if cancel {
        st.plan = None;
    }
    if write && let Some(plan) = st.plan.take() {
        st.message = Some(match install::apply(&plan) {
            Ok(backup) => {
                let done = if plan.install { "installed" } else { "removed" };
                let bak = backup.map(|b| format!(" (backup: {})", b.display())).unwrap_or_default();
                (true, format!("{} hooks {done}{bak}. New agent sessions pick them up.", plan.target.name()))
            }
            Err(e) => (false, e),
        });
        st.status = None;
    }
}
