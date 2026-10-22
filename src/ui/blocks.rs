//! Command blocks in the terminal view (Warp-style): a coloured mark in the left padding
//! of every prompt (green = success, red = failure, accent = running) with a hover card
//! and a block menu, faint separators between blocks, a sticky header naming the command
//! whose output fills the view, and Ctrl+Shift+Up/Down to step between prompts.
//!
//! The data lives in `shell_integration::blocks::ShellState` inside the terminal's
//! callbacks. [`BlocksView::sync`] runs under the terminal lock but copies the visible
//! blocks only when they changed (new marks or a scroll), so an idle frame costs a
//! couple of integer compares, and painting is a handful of rects.

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use crate::shell_integration::ShellIntegrationSettings;
use crate::shell_integration::blocks::{Block, Phase, ShellState};
use crate::ssh::terminal::TerminalHandle;
use crate::theme;

const BAR_W: f32 = 3.0;
const HEADER_PAD: f32 = 4.0;

/// The visible part of a block, copied out of the terminal state.
#[derive(Debug, Clone)]
pub struct Mark {
    pub id: u64,
    pub phase: Phase,
    pub prompt_line: u64,
    pub output_line: Option<u64>,
    pub end_line: Option<u64>,
    pub exit: Option<i32>,
    pub command: Option<Box<str>>,
    pub branch: Option<Arc<str>>,
    pub started: Option<Instant>,
    pub ended: Option<Instant>,
}

impl Mark {
    fn from_block(b: &Block) -> Self {
        Self {
            id: b.id,
            phase: b.phase,
            prompt_line: b.prompt_line,
            output_line: b.output_line,
            end_line: b.end_line,
            exit: b.exit,
            command: b.command.clone(),
            branch: b.branch.clone(),
            started: b.started,
            ended: b.ended,
        }
    }

    /// Only blocks that ran something get a mark.
    fn ran(&self) -> bool {
        self.output_line.is_some()
    }

    fn color(&self, accent: Color32) -> Color32 {
        match (self.phase, self.exit) {
            (Phase::Running, _) => accent,
            (_, Some(0)) => theme::success(),
            (_, Some(_)) => theme::danger(),
            _ => theme::text_faint(),
        }
    }

    fn duration(&self) -> Option<Duration> {
        let start = self.started?;
        Some(self.ended.unwrap_or_else(Instant::now).saturating_duration_since(start))
    }

    /// Exclusive end, or `None` while it runs.
    fn end(&self) -> Option<u64> {
        self.end_line
    }

    /// `cargo build · exit 1 · 2.3s · main`
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(4);
        parts.push(self.command.as_deref().map_or_else(|| "(command)".to_string(), |c| snippet(c, 80)));
        parts.push(match (self.phase, self.exit) {
            (Phase::Running, _) => "running".into(),
            (_, Some(code)) => format!("exit {code}"),
            _ => "exit ?".into(),
        });
        if let Some(d) = self.duration() {
            parts.push(format_duration(d));
        }
        if let Some(b) = &self.branch {
            parts.push(format!("on {b}"));
        }
        parts.join(" · ")
    }
}

fn snippet(text: &str, max: usize) -> String {
    let one_line = text.replace(['\n', '\r'], " ⏎ ");
    if one_line.chars().count() <= max {
        return one_line;
    }
    let cut: String = one_line.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// `0.4s`, `12.3s`, `4m 05s`, `1h 02m`.
pub fn format_duration(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 60.0 {
        format!("{s:.1}s")
    } else if s < 3600.0 {
        format!("{}m {:02}s", (s / 60.0) as u64, (s as u64) % 60)
    } else {
        format!("{}h {:02}m", (s / 3600.0) as u64, ((s as u64) / 60) % 60)
    }
}

/// What a frame asked for; resolved by [`BlocksView::apply`] (needs the terminal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Select(u64),
    CopyCommand(u64),
    CopyOutput(u64),
    CopyBoth(u64),
    Rerun(u64),
    /// Scroll so this absolute line is at the top.
    ScrollTo(u64),
    /// Ctrl+Shift+Up / Down.
    Step(bool),
}

/// Where the grid is painted this frame.
#[derive(Debug, Clone, Copy)]
pub struct Geom {
    pub rect: Rect,
    pub inner: Rect,
    pub cell_h: f32,
    pub rows: u16,
}

/// Per-view cache of the visible blocks plus selection state.
#[derive(Default)]
pub struct BlocksView {
    /// `(ShellState::version, top line, rows)` the marks were copied for.
    key: Option<(u64, u64, u16)>,
    /// Absolute line of row 0 when scrolled to the bottom (`ShellState::top_line(0)`).
    live_top: u64,
    top: u64,
    marks: Vec<Mark>,
    /// Block whose output covers the top row while its prompt is scrolled away.
    sticky: Option<Mark>,
    branch: Option<Arc<str>>,
    active: bool,
    pub selected: Option<u64>,
}

impl BlocksView {
    /// Under the terminal lock: refresh the copied marks when the blocks or the scroll
    /// position changed.
    pub fn sync(&mut self, shell: &ShellState, scroll: usize, rows: u16) {
        self.active = shell.active();
        if !self.active {
            return;
        }
        let top = shell.top_line(scroll);
        let key = (shell.version, top, rows);
        if self.key == Some(key) {
            return;
        }
        self.key = Some(key);
        self.live_top = shell.top_line(0);
        self.top = top;
        self.note_branch(shell);
        let blocks = shell.blocks();
        let bottom = top + u64::from(rows);
        let first = blocks.partition_point(|b| b.phase == Phase::Done && b.last_line() < top);
        self.marks.clear();
        self.marks.extend(blocks.range(first..).take_while(|b| b.prompt_line < bottom).map(Mark::from_block));
        self.sticky = blocks
            .range(..blocks.partition_point(|b| b.prompt_line < top))
            .next_back()
            .filter(|b| b.output_line.is_some_and(|o| o <= top) && b.end_line.is_none_or(|e| e > top + 1))
            .map(Mark::from_block);
        if self.selected.is_some_and(|id| shell.block(id).is_none()) {
            self.selected = None;
        }
    }

    /// Picks up a branch change without touching the marks (tab headers, any frame).
    pub fn note_branch(&mut self, shell: &ShellState) {
        if self.branch.as_deref() != shell.branch() {
            self.branch = shell.branch().map(Arc::from);
        }
    }

    /// Whether the shell has sent integration marks.
    pub fn active(&self) -> bool {
        self.active
    }

    /// Latest git branch reported by the shell.
    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }

    /// Paints marks, separators, the selection and the sticky header; returns what the
    /// user clicked.
    pub fn paint(&self, ui: &mut Ui, g: Geom, cfg: &ShellIntegrationSettings, accent: Color32) -> Option<Request> {
        if !self.active || self.marks.is_empty() && self.sticky.is_none() {
            return None;
        }
        let painter = ui.painter_at(g.rect);
        let row_y = |line: u64| g.inner.top() + (line as f64 - self.top as f64) as f32 * g.cell_h;
        let clip = |y: f32| y.clamp(g.inner.top(), g.inner.top() + f32::from(g.rows) * g.cell_h);
        let pad = g.inner.left() - g.rect.left();
        let bar_x = if pad >= BAR_W + 2.0 { g.rect.left() + (pad - BAR_W) * 0.5 } else { g.rect.left() + 1.0 };
        let mut request = None;
        for (i, mark) in self.marks.iter().enumerate() {
            let prompt_y = row_y(mark.prompt_line);
            if cfg.separators && mark.prompt_line > self.top && i + usize::from(self.sticky.is_some()) > 0 {
                let y = prompt_y.round() - 0.5;
                painter.line_segment([Pos2::new(g.inner.left(), y), Pos2::new(g.inner.right(), y)], Stroke::new(1.0, theme::text_faint().gamma_multiply(0.18)));
            }
            if !mark.ran() {
                continue;
            }
            let color = mark.color(accent);
            let head_end = mark.output_line.unwrap_or(mark.prompt_line + 1).max(mark.prompt_line + 1);
            let (y0, y1) = (clip(prompt_y), clip(row_y(head_end)));
            let selected = self.selected == Some(mark.id);
            if selected {
                let end = mark.end().unwrap_or(self.top + u64::from(g.rows));
                let sel = Rect::from_x_y_ranges(g.rect.left()..=g.inner.right(), clip(prompt_y)..=clip(row_y(end)));
                painter.rect_filled(sel, CornerRadius::same(theme::RADIUS_SM), accent.gamma_multiply(0.07));
            }
            if cfg.gutter_marks {
                if y1 > y0 {
                    painter.rect_filled(Rect::from_x_y_ranges(bar_x..=bar_x + BAR_W, y0..=y1), CornerRadius::same(2), color);
                }
                if let Some(out) = mark.output_line {
                    let end = mark.end().unwrap_or(self.top + u64::from(g.rows));
                    let (o0, o1) = (clip(row_y(out)), clip(row_y(end)));
                    if o1 > o0 {
                        let alpha = if selected { 0.6 } else { 0.22 };
                        painter.rect_filled(
                            Rect::from_x_y_ranges(bar_x + 0.5..=bar_x + BAR_W - 0.5, o0..=o1),
                            CornerRadius::same(1),
                            color.gamma_multiply(alpha),
                        );
                    }
                }
                if y1 > y0 {
                    let hit = Rect::from_x_y_ranges(g.rect.left()..=g.inner.left().max(g.rect.left() + 8.0), y0..=y1);
                    let resp = ui.interact(hit, ui.id().with(("block_mark", mark.id)), Sense::click());
                    if let Some(r) = mark_response(&resp, mark) {
                        request = Some(r);
                    }
                }
            }
        }
        if cfg.sticky_header
            && let Some(mark) = &self.sticky
            && let Some(r) = sticky_header(ui, &painter, g, mark, accent)
        {
            request = Some(r);
        }
        request
    }

    /// Carries out `req`. Returns bytes to type into the shell (Rerun), and updates the
    /// view's `scroll` for navigation.
    pub fn apply(&mut self, req: Request, handle: &TerminalHandle, ctx: &egui::Context, scroll: &mut usize) -> Option<Vec<u8>> {
        match req {
            Request::Select(id) => {
                self.selected = if self.selected == Some(id) { None } else { Some(id) };
                None
            }
            Request::ScrollTo(line) => {
                *scroll = self.live_top.saturating_sub(line) as usize;
                None
            }
            Request::Step(up) => {
                let from = self.selected.and_then(|id| self.marks.iter().chain(self.sticky.iter()).find(|m| m.id == id).map(|m| m.prompt_line));
                let (target, live_top) = step(handle, from.unwrap_or(self.top), up, *scroll == 0 && from.is_none())?;
                self.live_top = live_top;
                match target {
                    Some((id, line)) => {
                        self.selected = Some(id);
                        *scroll = live_top.saturating_sub(line) as usize;
                    }
                    None => {
                        self.selected = None;
                        *scroll = 0;
                    }
                }
                None
            }
            Request::CopyCommand(id) | Request::CopyOutput(id) | Request::CopyBoth(id) => {
                let (command, output) = block_text(handle, id)?;
                let text = match req {
                    Request::CopyCommand(_) => command,
                    Request::CopyOutput(_) => output,
                    _ if output.is_empty() => command,
                    _ => format!("{command}\n{output}"),
                };
                ctx.copy_text(text);
                None
            }
            Request::Rerun(id) => {
                let command = handle.shared.lock().ok()?.parser.callbacks().shell.block(id)?.command.clone()?;
                // Bracketed paste would leave it waiting for Enter in some shells: type it.
                Some(format!("{}\r", command.replace(['\r', '\n'], " ")).into_bytes())
            }
        }
    }
}

/// Hover card, click to select, right-click menu.
fn mark_response(resp: &egui::Response, mark: &Mark) -> Option<Request> {
    if resp.hovered() {
        resp.ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let resp = resp.clone().on_hover_text(mark.summary());
    let mut request = resp.clicked().then_some(Request::Select(mark.id));
    resp.context_menu(|ui| {
        if ui.button("Copy command").clicked() {
            request = Some(Request::CopyCommand(mark.id));
            ui.close();
        }
        if ui.button("Copy output").clicked() {
            request = Some(Request::CopyOutput(mark.id));
            ui.close();
        }
        if ui.button("Copy command + output").clicked() {
            request = Some(Request::CopyBoth(mark.id));
            ui.close();
        }
        ui.separator();
        let can_rerun = mark.command.is_some() && mark.phase == Phase::Done;
        if ui.add_enabled(can_rerun, egui::Button::new("Rerun")).clicked() {
            request = Some(Request::Rerun(mark.id));
            ui.close();
        }
    });
    request
}

/// The command whose output fills the view, pinned over the first row.
fn sticky_header(ui: &mut Ui, painter: &egui::Painter, g: Geom, mark: &Mark, accent: Color32) -> Option<Request> {
    let h = g.cell_h + HEADER_PAD * 2.0;
    let bar = Rect::from_min_size(Pos2::new(g.rect.left(), g.rect.top()), Vec2::new(g.rect.width(), h));
    let resp = ui.interact(bar, ui.id().with("block_sticky"), Sense::click());
    // Opaque: the row underneath must not show through.
    let fill = theme::term_bg().lerp_to_gamma(theme::surface_3(), if resp.hovered() { 1.0 } else { 0.75 });
    painter.rect_filled(bar, CornerRadius { nw: theme::RADIUS, ne: theme::RADIUS, sw: 0, se: 0 }, fill.to_opaque());
    painter.line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, theme::border()));
    let color = mark.color(accent);
    let pad = g.inner.left() - g.rect.left();
    let bar_x = if pad >= BAR_W + 2.0 { g.rect.left() + (pad - BAR_W) * 0.5 } else { g.rect.left() + 1.0 };
    painter.rect_filled(Rect::from_x_y_ranges(bar_x..=bar_x + BAR_W, bar.top() + HEADER_PAD..=bar.bottom() - HEADER_PAD), CornerRadius::same(2), color);
    let font = FontId::new((g.cell_h * 0.72).clamp(9.0, 20.0), FontFamily::Monospace);
    let status = match (mark.phase, mark.exit) {
        (Phase::Running, _) => format!("running {}", mark.duration().map(format_duration).unwrap_or_default()),
        (_, Some(code)) => format!("exit {code}"),
        _ => String::new(),
    };
    // The status follows the command (the right edge belongs to the "lines up" pill).
    let max_chars = (((g.inner.width() * 0.6) / (font.size * 0.6)).max(8.0)) as usize;
    let text = format!("❯ {}", snippet(mark.command.as_deref().unwrap_or(""), max_chars));
    let text_w = painter.text(Pos2::new(g.inner.left(), bar.center().y), Align2::LEFT_CENTER, text, font, theme::text()).width();
    painter.text(Pos2::new(g.inner.left() + text_w + 12.0, bar.center().y), Align2::LEFT_CENTER, status, FontId::proportional(11.0), color);
    if mark.phase == Phase::Running {
        ui.ctx().request_repaint_after(Duration::from_secs(1));
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text("Scroll to this command").clicked().then_some(Request::ScrollTo(mark.prompt_line))
}

/// The "previous / next prompt" shortcuts (Ctrl+Shift+Up / Down by default) in a terminal.
pub fn step_key(key: egui::Key, mods: egui::Modifiers) -> Option<bool> {
    use crate::keybindings::{self, Action};
    if keybindings::is_recording() {
        return None;
    }
    let is = |a: Action| keybindings::binding(a).is_some_and(|b| b.matches(key, mods));
    if is(Action::PrevPrompt) {
        Some(true)
    } else if is(Action::NextPrompt) {
        Some(false)
    } else {
        None
    }
}

/// The prompt before (`up`) or after `from`; `None` inside means "back to the live
/// screen". At the live screen with nothing selected, "up" starts from the current prompt.
fn step(handle: &TerminalHandle, from: u64, up: bool, at_live: bool) -> Option<(Option<(u64, u64)>, u64)> {
    let shared = handle.shared.lock().ok()?;
    let shell = &shared.parser.callbacks().shell;
    let blocks = shell.blocks();
    let live_top = shell.top_line(0);
    let ran = |b: &&Block| b.output_line.is_some();
    let from = if at_live { blocks.back().map_or(from, |b| b.prompt_line) } else { from };
    let target = if up { blocks.iter().rev().filter(ran).find(|b| b.prompt_line < from) } else { blocks.iter().filter(ran).find(|b| b.prompt_line > from) };
    Some((target.map(|b| (b.id, b.prompt_line)), live_top))
}

/// Command line and output text of block `id`.
fn block_text(handle: &TerminalHandle, id: u64) -> Option<(String, String)> {
    let mut shared = handle.shared.lock().ok()?;
    let shell = &shared.parser.callbacks().shell;
    let block = shell.block(id)?;
    let map = shell.line_map();
    let (cmd_from, cmd_to) = block.command_range();
    let typed = block.command.as_deref().map(str::to_string);
    let output = block.output_range().map(|(from, to)| (from, to.unwrap_or(u64::MAX)));
    let screen = shared.parser.screen_mut();
    let command = typed.unwrap_or_else(|| map.text(screen, cmd_from, cmd_to));
    let output = output.map(|(from, to)| map.text(screen, from, to)).unwrap_or_default();
    Some((command, output))
}

/// A finished command worth telling the user about: ran at least `cfg.notify_after()`.
/// Returns (title, body).
pub fn long_command_notice(done: &crate::shell_integration::blocks::Finished, cfg: &ShellIntegrationSettings) -> Option<(String, String)> {
    if !cfg.notify_long_commands || done.duration < cfg.notify_after() {
        return None;
    }
    let status = match done.exit {
        Some(0) => "succeeded".to_string(),
        Some(code) => format!("failed (exit {code})"),
        None => "finished".to_string(),
    };
    let title = format!("Command {status}");
    let body = format!("{} · {}", done.command.as_deref().map_or_else(|| "(command)".to_string(), |c| snippet(c, 120)), format_duration(done.duration));
    Some((title, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Profile;
    use crate::shell_integration::blocks::{Finished, escape};
    use crate::ssh::terminal::{LinkState, feed};
    use crate::ui::terminal_view::TerminalView;

    fn run(cmd: &str, output: &[&str], exit: i32) -> String {
        let mut s = format!(
            "\x1b]6973;branch;main\x07\x1b]133;A\x07\x1b[32m~/tether\x1b[0m \x1b[35m(main)\x1b[0m $ \x1b]133;B\x07{cmd}\r\n\x1b]6973;cmd;{}\x07\x1b]133;C\x07",
            escape(cmd)
        );
        for line in output {
            s.push_str(line);
            s.push_str("\r\n");
        }
        s.push_str(&format!("\x1b]133;D;{exit}\x07"));
        s
    }

    fn demo_terminal(long: bool) -> TerminalHandle {
        let (handle, _rx) = TerminalHandle::new(24, 90);
        if let Ok(mut s) = handle.shared.lock() {
            s.state = LinkState::Connected;
            let mut text = run("git status --short", &[" M src/ui/blocks.rs", "?? src/shell_integration/"], 0);
            text += &run("cargo test --lib blocks", &["running 23 tests", "test result: FAILED. 22 passed; 1 failed"], 101);
            if long {
                let lines: Vec<String> = (0..60).map(|i| format!("   Compiling crate-{i:02} v0.1.{i}")).collect();
                let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
                text += &run("cargo build --release", &refs, 0);
            }
            text += &run("ls", &["Cargo.toml  README.md  src  tests"], 0);
            text += "\x1b]133;A\x07\x1b[32m~/tether\x1b[0m \x1b[35m(main)\x1b[0m $ \x1b]133;B\x07\x1b]133;C\x07sleep 30\r\n";
            feed(&mut s.parser, text.as_bytes());
        }
        handle
    }

    /// Gutter marks, separators, a hover card and the sticky header, written to
    /// `target/ui-shots/terminal-blocks*.png`.
    #[test]
    #[ignore = "writes target/ui-shots/terminal-blocks*.png (needs a GPU adapter)"]
    fn blocks_screenshots() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/ui-shots");
        std::fs::create_dir_all(&dir).unwrap();
        for (name, long, scroll, hover) in [("terminal-blocks", false, 0, true), ("terminal-blocks-sticky", true, 30, false)] {
            let mut view = TerminalView::new(demo_terminal(long));
            view.set_scroll(scroll);
            let profile = Profile::new();
            let mut harness = egui_kittest::Harness::builder().with_size(Vec2::new(900.0, 460.0)).wgpu().build_ui(move |ui| {
                ui.ctx().all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
                view.ui(ui, &profile);
            });
            harness.run_steps(4);
            if hover {
                // The second block's mark (rows 3-4 at the default 10 px padding).
                let y = 10.0 + 3.5 * 17.0;
                harness.input_mut().events.push(egui::Event::PointerMoved(Pos2::new(5.0, y)));
                harness.run_steps(6);
            }
            harness.render().expect("render").save(dir.join(format!("{name}.png"))).unwrap();
        }
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(format_duration(Duration::from_millis(420)), "0.4s");
        assert_eq!(format_duration(Duration::from_secs(245)), "4m 05s");
        assert_eq!(format_duration(Duration::from_secs(3720)), "1h 02m");
    }

    #[test]
    fn step_keys() {
        let cs = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        assert_eq!(step_key(egui::Key::ArrowUp, cs), Some(true));
        assert_eq!(step_key(egui::Key::ArrowDown, cs), Some(false));
        assert_eq!(step_key(egui::Key::ArrowUp, egui::Modifiers::CTRL), None);
    }

    #[test]
    fn long_commands_notify_only_past_the_threshold() {
        let cfg = ShellIntegrationSettings::default();
        let quick = Finished { command: Some("ls".into()), exit: Some(0), duration: Duration::from_secs(1) };
        assert!(long_command_notice(&quick, &cfg).is_none());
        let slow = Finished { command: Some("cargo build".into()), exit: Some(101), duration: Duration::from_secs(42) };
        let (title, body) = long_command_notice(&slow, &cfg).unwrap();
        assert_eq!(title, "Command failed (exit 101)");
        assert_eq!(body, "cargo build · 42.0s");
        let off = ShellIntegrationSettings { notify_long_commands: false, ..cfg };
        assert!(long_command_notice(&slow, &off).is_none());
    }

    #[test]
    fn summary_line() {
        let m = Mark {
            id: 1,
            phase: Phase::Done,
            prompt_line: 0,
            output_line: Some(1),
            end_line: Some(3),
            exit: Some(1),
            command: Some("make test".into()),
            branch: Some(Arc::from("main")),
            started: None,
            ended: None,
        };
        assert_eq!(m.summary(), "make test · exit 1 · on main");
    }
}
