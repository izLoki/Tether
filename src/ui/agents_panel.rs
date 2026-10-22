//! The "Agents" sidebar section: every AI agent session across tabs and windows, most urgent
//! first, with what it is doing, for how long, its last message and its CPU / memory.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, CornerRadius, FontId, Rect, RichText, Sense, Vec2};

use super::agent_badge;
use crate::agents::{AgentKind, AgentState, SessionInfo, launch, snippet};
use crate::procmon::human_mem;
use crate::theme;

pub const WIDTH: f32 = 300.0;
pub fn default_command(kind: AgentKind) -> &'static str {
    kind.id()
}

pub struct CustomBoot {
    pub kind: AgentKind,
    command: String,
    shell_id: String,
    focus: bool,
    custom: bool,
    project: super::project_picker::ProjectPicker,
    pub target: Option<egui_dock::NodePath>,
}

impl CustomBoot {
    pub fn new(kind: AgentKind) -> Self {
        let settings = crate::settings::snapshot();
        let shell = settings.agents.launch_shells.get(kind.id()).and_then(|id| crate::pty::shells::find(id)).or_else(crate::pty::shells::default_shell);
        Self {
            kind,
            command: default_command(kind).into(),
            shell_id: shell.map(|s| s.id.clone()).unwrap_or_default(),
            focus: true,
            custom: true,
            project: Default::default(),
            target: None,
        }
    }

    pub fn launch(pick: LaunchPick, target: egui_dock::NodePath) -> Self {
        let mut boot = Self::new(pick.kind);
        boot.command = pick.command;
        boot.custom = false;
        boot.target = Some(target);
        if let Some(shell) = pick.shell_id {
            boot.shell_id = shell;
        }
        boot
    }

    pub fn show(&mut self, ctx: &egui::Context) -> (Option<LaunchPick>, bool) {
        let mut command = None;
        let mut close = false;
        use super::form::{self, Kind};
        let modal = egui::Modal::new(egui::Id::new("custom_agent_boot")).frame(form::modal_frame()).show(ctx, |ui| {
            egui::Frame::new().inner_margin(24).show(ui, |ui| {
                ui.set_width(540.0);
                ui.horizontal(|ui| {
                    super::agent_icons::icon(ui, self.kind, 36.0);
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(format!("{} · {}", self.kind.name(), if self.custom { "Custom Boot" } else { "Launch" })).size(19.0).strong());
                        ui.label(RichText::new("Your command, in a new terminal session.").size(12.0).color(theme::text_dim()));
                    });
                });
                ui.add_space(20.0);
                let response = form::section(
                    ui,
                    Some(super::settings_widgets::Icon::Terminal),
                    "Launch command",
                    "Run your local command with its own flags and account setup.",
                    |ui| {
                        let width = ui.available_width();
                        form::framed(ui, false, |ui| {
                            ui.add_enabled(self.custom, form::style_edit(egui::TextEdit::singleline(&mut self.command), width).font(egui::TextStyle::Monospace))
                        })
                    },
                );
                if std::mem::take(&mut self.focus) {
                    if self.custom {
                        response.request_focus();
                    } else {
                        self.project.focus(ctx);
                    }
                }
                ui.add_space(12.0);
                self.project.show(ui);
                ui.add_space(12.0);
                form::label(ui, "Shell", false);
                let shells = crate::pty::shells::detected().unwrap_or_default();
                let shown = shells.iter().find(|s| s.id == self.shell_id).map_or("Default shell", |s| s.name.as_str());
                let shell_response = form::select(ui, ui.id().with("boot_shell"), ui.available_width(), shown, |ui| {
                    for shell in shells {
                        if form::option(ui, &shell.name, None, shell.id == self.shell_id).clicked() {
                            self.shell_id.clone_from(&shell.id);
                            ui.close();
                        }
                    }
                });
                shell_response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, "Terminal shell"));
                ui.add_space(6.0);
                form::note(ui, "Uses this shell's profiles, functions and aliases. Your command and flags are passed through unchanged.", false);
                let valid = !self.command.trim().is_empty() && !self.command.contains(['\r', '\n', '\0']);
                ui.add_space(20.0);
                form::rule(ui);
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if form::button(ui, "Start terminal", Kind::Primary, valid).clicked() || (valid && enter) {
                        if let Some(cwd) = self.project.directory() {
                            command = Some(LaunchPick {
                                kind: self.kind,
                                command: self.command.trim().to_owned(),
                                shell_id: Some(self.shell_id.clone()),
                                cwd: Some(cwd),
                            });
                            close = true;
                        }
                    }
                    if form::button(ui, "Cancel", Kind::Secondary, true).clicked() {
                        close = true;
                    }
                });
            });
        });
        (command, close || modal.should_close())
    }
}
const ROW_H: f32 = 64.0;

pub enum PanelAction {
    Focus(u64),
    Close,
    /// Start an agent from the empty state.
    Launch(LaunchPick),
    /// Settings › AI agents (hook installer).
    InstallHooks,
    CustomBoot(AgentKind),
}

/// An agent picked from a "+" menu.
#[derive(Debug, Clone)]
pub struct LaunchPick {
    pub kind: AgentKind,
    pub command: String,
    /// A chosen local shell, including its normal startup profiles.
    pub shell_id: Option<String>,
    /// A confirmed directory; absent means show the shared launch form first.
    pub cwd: Option<std::path::PathBuf>,
}

/// "Claude Code" / "Codex" entries for a "+" menu: only agents found on PATH.
pub fn launch_menu(ui: &mut egui::Ui) -> Option<LaunchPick> {
    let found = launch::available();
    if found.is_empty() {
        return None;
    }
    ui.separator();
    let mut pick = None;
    for l in found {
        ui.horizontal(|ui| {
            let text = RichText::new(format!("{}  {}", l.kind.glyph(), l.kind.name())).color(theme::hue_color(l.kind.hue()));
            if ui.button(text).on_hover_text(format!("Choose a project and shell for {}\n{}", l.command, l.path.display())).clicked() {
                pick = Some(LaunchPick { kind: l.kind, command: l.command.into(), shell_id: None, cwd: None });
            }
        });
    }
    if pick.is_some() {
        ui.close();
    }
    pick
}

/// `42s`, `5m`, `2h 07m`.
pub fn elapsed(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// Sessions waiting for the user (the section header shows the count).
pub fn waiting(sessions: &[SessionInfo]) -> usize {
    sessions.iter().filter(|s| s.state.needs_attention() && s.state != AgentState::Done).count()
}

/// Height the session list wants (the sidebar section sizes itself with it).
pub fn list_height(sessions: &[SessionInfo]) -> f32 {
    if sessions.is_empty() { EMPTY_H } else { sessions.len() as f32 * (ROW_H + 4.0) }
}

const EMPTY_H: f32 = 172.0;

/// The sessions without a header (the sidebar's "Agents" section draws its own).
pub fn list(ui: &mut egui::Ui, sessions: &[SessionInfo], now: Instant) -> Option<PanelAction> {
    list_for(ui, sessions, crate::views::View::Agents, now)
}

pub fn list_for(ui: &mut egui::Ui, sessions: &[SessionInfo], view: crate::views::View, now: Instant) -> Option<PanelAction> {
    let mut action = None;
    let mut sessions = sessions.iter().filter(|s| view.includes_agent(s.kind)).peekable();
    if sessions.peek().is_none() {
        return empty_state(ui, view.agent().unwrap_or(AgentKind::Claude));
    }
    let time = ui.input(|i| i.time);
    egui::ScrollArea::vertical().id_salt(("agents_list", view)).auto_shrink([false, false]).show(ui, |ui| {
        for s in sessions {
            if row(ui, s, now, time) {
                action = Some(PanelAction::Focus(s.uid));
            }
            ui.add_space(4.0);
        }
    });
    action
}

fn empty_state(ui: &mut egui::Ui, kind: AgentKind) -> Option<PanelAction> {
    use super::form::{self, Kind};
    let mut action = None;
    ui.add_space(4.0);
    egui::Frame::new()
        .fill(theme::surface_2().gamma_multiply(0.7))
        .stroke(egui::Stroke::new(1.0, theme::border().gamma_multiply(0.6)))
        .corner_radius(10)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            super::agent_icons::icon(ui, kind, 30.0);
            ui.add_space(8.0);
            ui.label(RichText::new(format!("{} sessions", kind.name())).size(14.0).strong());
            ui.label(RichText::new("Follow activity, approvals and resource use across your local terminals.").size(12.0).color(theme::text_dim()));
            ui.add_space(12.0);
            if form::button(ui, &format!("Launch {}", kind.name()), Kind::Primary, true).clicked() {
                action = Some(PanelAction::Launch(LaunchPick { kind, command: default_command(kind).into(), shell_id: None, cwd: None }));
            }
            ui.add_space(4.0);
            if form::button(ui, "Custom Boot…", Kind::Secondary, true).clicked() {
                action = Some(PanelAction::CustomBoot(kind));
            }
            ui.add_space(4.0);
            if form::button(ui, "Install hooks…", Kind::Ghost, true).clicked() {
                action = Some(PanelAction::InstallHooks);
            }
        });
    action
}

/// One session card. Returns true when clicked.
fn row(ui: &mut egui::Ui, s: &SessionInfo, now: Instant, time: f64) -> bool {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, ROW_H), Sense::click());
    let resp = resp.on_hover_text(hover_text(s));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let p = ui.painter();
    let attention = s.state.needs_attention() && s.state != AgentState::Done;
    let fill = if resp.hovered() { theme::surface_2() } else { theme::surface() };
    p.rect_filled(rect, CornerRadius::same(theme::RADIUS), fill);
    if attention {
        let bar = Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height()));
        p.rect_filled(bar, CornerRadius { nw: theme::RADIUS, sw: theme::RADIUS, ne: 0, se: 0 }, agent_badge::color(s.state));
    }
    let left = rect.left() + 14.0;
    agent_badge::paint(p, egui::pos2(left + 6.0, rect.top() + 18.0), 6.0, s.state, time);
    let title_pos = egui::pos2(left + 20.0, rect.top() + 18.0);
    let icon = Rect::from_center_size(egui::pos2(title_pos.x + 7.0, title_pos.y), Vec2::splat(14.0));
    super::agent_icons::paint(p, icon, s.kind, 1.0);
    let name_w = rect.right() - icon.right() - 80.0;
    let title = clip(&s.title, name_w, 12.5);
    p.text(egui::pos2(icon.right() + 6.0, title_pos.y), Align2::LEFT_CENTER, title, FontId::proportional(12.5), theme::text());
    let age = elapsed(now.saturating_duration_since(s.since));
    p.text(egui::pos2(rect.right() - 10.0, title_pos.y), Align2::RIGHT_CENTER, age, FontId::proportional(10.5), theme::text_faint());

    let line2 = egui::pos2(left + 20.0, rect.top() + 36.0);
    let state = p.text(line2, Align2::LEFT_CENTER, s.state.label(), FontId::proportional(11.0), agent_badge::color(s.state));
    if s.project != s.title {
        let project = clip(&s.project, rect.right() - state.right() - 100.0, 11.0);
        p.text(egui::pos2(state.right() + 6.0, line2.y), Align2::LEFT_CENTER, format!("· {project}"), FontId::proportional(11.0), theme::text_faint());
    }
    if let Some(u) = &s.usage {
        let usage = format!("{:.0}% · {}", u.cpu, human_mem(u.mem));
        p.text(egui::pos2(rect.right() - 10.0, line2.y), Align2::RIGHT_CENTER, usage, FontId::proportional(10.5), theme::text_faint());
    }
    if let Some(m) = &s.message {
        let text = clip(&snippet(m, 120), rect.width() - 44.0, 11.0);
        p.text(egui::pos2(left + 20.0, rect.top() + 52.0), Align2::LEFT_CENTER, text, FontId::proportional(11.0), theme::text_dim());
    }
    resp.clicked()
}

fn hover_text(s: &SessionInfo) -> String {
    let mut t = format!("{} · {}", s.kind.name(), s.state.label());
    if let Some(cwd) = &s.cwd {
        t.push_str(&format!("\n{cwd}"));
    }
    if let Some(m) = &s.message {
        t.push_str(&format!("\n\n{}", snippet(m, 400)));
    }
    t.push_str(match (s.kind, s.hooked) {
        (AgentKind::Codex, true) => "\n\nCompletion from Codex notify; activity from the terminal",
        (_, true) => "\n\nState from hooks",
        _ => "\n\nState from terminal activity (configure agent signals in Settings)",
    });
    t.push_str("\nClick to focus the tab");
    t
}

/// Cuts `text` to roughly `width` points (proportional font estimate).
fn clip(text: &str, width: f32, size: f32) -> String {
    let max = ((width / (size * 0.55)).floor() as usize).max(4);
    snippet(text, max)
}

#[cfg(test)]
mod custom_boot_tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    #[test]
    fn local_account_flags_pass_through_the_form_without_a_prompt() {
        let result = std::sync::Arc::new(std::sync::Mutex::new(None));
        let output = result.clone();
        let mut boot = CustomBoot {
            kind: AgentKind::Claude,
            command: "claude -p".into(),
            shell_id: "powershell".into(),
            focus: false,
            custom: true,
            project: Default::default(),
            target: None,
        };
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 950.0)).build_ui(move |ui| {
            if output.lock().unwrap().is_none() {
                let (pick, _) = boot.show(ui.ctx());
                if let Some(pick) = pick {
                    *output.lock().unwrap() = Some(pick);
                }
            }
        });
        h.run_steps(5);
        assert!(h.query_by_label("Initial prompt").is_none());
        h.get_by_label("Start terminal").click();
        h.run_steps(5);
        let pick = result.lock().unwrap().take().expect("custom command was accepted");
        assert_eq!(pick.command, "claude -p");
        assert_eq!(pick.shell_id.as_deref(), Some("powershell"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_formats() {
        assert_eq!(elapsed(Duration::from_secs(42)), "42s");
        assert_eq!(elapsed(Duration::from_secs(300)), "5m");
        assert_eq!(elapsed(Duration::from_secs(7620)), "2h 07m");
    }
}
