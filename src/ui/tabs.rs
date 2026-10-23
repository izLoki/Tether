//! Dock tabs (terminal / SFTP / editor) and the egui_dock TabViewer that renders them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use eframe::egui::{self, Id, RichText, Ui, WidgetText};
use egui_dock::tab_viewer::OnCloseResponse;
use egui_dock::{DockState, Node, Split};

use super::editor_tab::EditorView;
use super::sftp_view::SftpView;
use super::terminal_view::TerminalView;
use super::vertical_tabs::TabAction;
use super::{agent_badge, agent_icons, agent_status, usage_chip};
use crate::agents::{AgentKind, AgentState};
use crate::commands::{self, AppCommand, EditorDoc};
use crate::profile::Profile;
use crate::pty::{self, ShellSpec};
use crate::ssh::terminal::LinkState;
use crate::theme;
use crate::workspace::{TabKind, TabSpec};

static NEXT_UID: AtomicU64 = AtomicU64::new(1);
const APPEAR_SECS: f64 = 0.28;

pub fn next_uid() -> u64 {
    NEXT_UID.fetch_add(1, Ordering::Relaxed)
}

pub enum TabBody {
    Terminal(Box<TerminalView>),
    Sftp(Box<SftpView>),
    /// A remote file open in the built-in editor. Not saved in workspaces.
    Editor(Box<EditorView>),
}

/// An SFTP browser linked directly to the right side of an SSH terminal tab so it
/// only shows while that SSH terminal tab is active, unless dragged/popped out.
pub struct LinkedSftp {
    pub view: Box<SftpView>,
    pub split_fraction: f32,
}

impl LinkedSftp {
    pub fn new(view: SftpView) -> Self {
        Self { view: Box::new(view), split_fraction: 0.54 }
    }
}

/// User action on a terminal tab's linked right-hand SFTP pane.
pub enum LinkedSftpAction {
    /// Separate the linked SFTP into an independent floating window at `pos`.
    Detach { uid: u64, pos: Option<egui::Pos2>, rect: egui::Rect },
    /// Minimize the linked SFTP into the mini-dock bar with the Genie animation.
    Minimize { uid: u64, rect: egui::Rect },
}

pub struct Tab {
    pub uid: u64,
    /// The host; for a local shell, a stand-in (`pty::local_profile`) with a nil id.
    pub profile: Profile,
    pub body: TabBody,
    pub born: Option<f64>,
    /// Set for local shells: SSH-only features (SFTP links, cwd probes, `nano` hand-off)
    /// skip these tabs, and reconnecting respawns the shell.
    pub local: Option<ShellSpec>,
    /// A name the user gave the tab; replaces the automatic title.
    pub custom_title: Option<String>,
    /// Optional companion SFTP pane linked to this SSH terminal instance.
    pub linked_sftp: Option<LinkedSftp>,
}

impl Tab {
    pub fn new(profile: Profile, body: TabBody) -> Self {
        Self { uid: next_uid(), profile, body, born: None, local: None, custom_title: None, linked_sftp: None }
    }

    pub fn new_local(spec: ShellSpec, view: TerminalView) -> Self {
        Self { local: Some(spec.clone()), ..Self::new(pty::local_profile(&spec), TabBody::Terminal(Box::new(view))) }
    }

    pub fn is_local(&self) -> bool {
        self.local.is_some()
    }

    /// Best-known working directory for this tab (AI agent hook -> OSC 7 -> live OS process
    /// CWD -> initial fallback CWD).
    pub fn current_cwd(&self) -> Option<String> {
        let TabBody::Terminal(t) = &self.body else { return None };
        if self.is_local()
            && let Some(cwd) = crate::agents::hub::agent_cwd(self.uid)
        {
            return Some(cwd);
        }
        let osc_cwd = t.handle.shared.try_lock().ok().and_then(|s| s.parser.callbacks().cwd.clone()).or_else(|| t.remote_cwd().map(str::to_string));
        if osc_cwd.is_some() {
            return osc_cwd;
        }
        if self.is_local()
            && let Some(cwd) = crate::procmon::usage_for(self.uid).and_then(|u| u.cwd.clone())
        {
            return Some(cwd);
        }
        t.fallback_cwd.clone()
    }

    pub fn spec(&self) -> TabSpec {
        let cwd = self.current_cwd();
        if let Some(shell) = &self.local {
            return TabSpec { title: self.custom_title.clone(), cwd, ..TabSpec::new(uuid::Uuid::nil(), TabKind::Local { shell_id: shell.id.clone() }) };
        }
        let kind = match self.body {
            TabBody::Terminal(_) => TabKind::Terminal,
            // Editors are skipped by workspaces (see `is_persistable`); reporting them as
            // SFTP keeps the host's connection counted as in use while a file is open.
            TabBody::Sftp(_) | TabBody::Editor(_) => TabKind::Sftp,
        };
        let (sftp_local_cwd, sftp_show_local) = match (&self.body, &self.linked_sftp) {
            (TabBody::Sftp(view), _) => (view.local_cwd_str(), Some(view.shows_local())),
            (TabBody::Terminal(_), Some(linked)) => (linked.view.local_cwd_str(), Some(linked.view.shows_local())),
            _ => (None, None),
        };
        TabSpec {
            title: self.custom_title.clone(),
            cwd,
            with_sftp: self.linked_sftp.is_some(),
            sftp_local_cwd,
            sftp_show_local,
            ..TabSpec::new(self.profile.id, kind)
        }
    }

    /// Whether the tab belongs in a saved workspace layout (editor tabs do not).
    pub fn is_persistable(&self) -> bool {
        !matches!(self.body, TabBody::Editor(_))
    }

    pub fn is_editor(&self) -> bool {
        matches!(self.body, TabBody::Editor(_))
    }

    /// True for the editor tab of `remote_path` on host `profile_id`.
    pub fn edits(&self, profile_id: uuid::Uuid, remote_path: &str) -> bool {
        matches!(&self.body, TabBody::Editor(e) if e.edits(profile_id, remote_path))
    }

    /// Sets or clears (`None` / blank) the user's name for the tab.
    pub fn rename(&mut self, title: Option<String>) {
        self.custom_title = title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    }

    /// Glyph shown before the title.
    pub fn icon(&self) -> &'static str {
        match (&self.body, &self.local) {
            (TabBody::Terminal(_), Some(shell)) => shell.icon_kind.glyph(),
            (TabBody::Terminal(_), None) => "⌨",
            (TabBody::Sftp(_), _) => "📁",
            (TabBody::Editor(_), _) => "📝",
        }
    }

    /// The title tabs show: the user's name, else what the shell / file reports, else
    /// the host's name. Refreshes the terminal's cached title, hence `&mut`.
    /// A local shell running an AI agent is named after the agent's session
    /// (`agents::title`) and reverts once the agent quits.
    pub fn display_title(&mut self) -> String {
        let agent = self.agent().map(|b| b.kind);
        let remote_title = match (&mut self.body, &self.local) {
            (TabBody::Terminal(t), Some(shell)) => {
                let title = t.remote_title();
                let cwd = t.remote_cwd();
                // The agent's own folder (from its hooks) names the project best.
                let agent_cwd = agent.and_then(|_| crate::agents::hub::agent_cwd(self.uid));
                let title_cwd = agent_cwd.as_deref().or(cwd);
                return crate::agents::title::tab_title(self.custom_title.as_deref(), agent, title.as_deref(), title_cwd, |raw| {
                    pty::tab_title(shell, raw, cwd)
                });
            }
            (TabBody::Terminal(t), None) => t.remote_title(),
            (TabBody::Sftp(_), _) => None,
            (TabBody::Editor(e), _) => Some(if e.is_dirty() { format!("{} •", e.file_name()) } else { e.file_name().to_string() }),
        };
        if let Some(custom) = &self.custom_title {
            return custom.clone();
        }
        let name = remote_title.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| self.profile.display_name().to_string());
        super::agent_badge::clean_title(&name).to_string()
    }

    /// Second line in the vertical tab list: folder for local shells (else the shell's
    /// full name), address for remote ones, the file path for editors.
    pub fn subtitle(&self) -> String {
        match (&self.body, &self.local) {
            (TabBody::Terminal(t), Some(shell)) => {
                if let Some((cwd, branch)) = t.shell_context() {
                    context_line(&short_path(cwd), branch)
                } else if let Some(cwd) = self.current_cwd() {
                    short_path(&cwd)
                } else {
                    shell.name.clone()
                }
            }
            (TabBody::Terminal(_), None) => self.profile.address(),
            (TabBody::Sftp(_), _) => format!("SFTP · {}", self.profile.address()),
            (TabBody::Editor(e), _) => e.remote_path().to_string(),
        }
    }

    /// The AI agent running in this local shell (whatever the badge setting).
    pub fn agent(&self) -> Option<crate::agents::Badge> {
        if self.is_local() { crate::agents::hub::badge_for(self.uid) } else { None }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self.body, TabBody::Terminal(_))
    }

    pub fn status_color(&self) -> egui::Color32 {
        match &self.body {
            TabBody::Terminal(t) => match t.link_state() {
                LinkState::Connected => theme::accent(),
                LinkState::Connecting => theme::warn(),
                _ => theme::danger(),
            },
            TabBody::Sftp(s) if s.is_ready() => theme::blue(),
            TabBody::Sftp(_) => theme::warn(),
            TabBody::Editor(e) => e.status_color(),
        }
    }
}

/// Applies the queued [`AppCommand`]s to the dock. Call once per frame.
pub fn apply_commands(dock: &mut DockState<Tab>, rt: &tokio::runtime::Handle, ctx: &egui::Context) {
    for cmd in commands::drain() {
        match cmd {
            AppCommand::OpenEditor(doc) => place_editor(dock, *doc, rt, ctx),
            AppCommand::CloseTab(uid) => {
                if let Some(path) = dock.find_tab_from(|t| t.uid == uid) {
                    dock.remove_tab(path);
                }
            }
        }
    }
}

/// Focuses the editor already showing the file, else joins the leaf holding other editors,
/// else opens it below the whole workspace when already split (or to the right of a single pane).
fn place_editor(dock: &mut DockState<Tab>, doc: EditorDoc, rt: &tokio::runtime::Handle, ctx: &egui::Context) {
    if let Some(path) = dock.find_tab_from(|t| t.edits(doc.profile.id, &doc.remote_path)) {
        let _ = dock.set_active_tab(path);
        dock.set_focused_node_and_surface(path.node_path());
        return;
    }
    let tab = Tab::new(doc.profile.clone(), TabBody::Editor(Box::new(EditorView::new(doc, rt.clone(), ctx.clone()))));
    if let Some(path) = dock.find_tab_from(Tab::is_editor)
        && let Ok(leaf) = dock.leaf_mut(path.node_path())
    {
        leaf.append_tab(tab);
        dock.set_focused_node_and_surface(path.node_path());
        return;
    }
    if dock.main_surface().root_node().is_some_and(Node::is_parent) {
        let [_, new] = dock.main_surface_mut().split_below(egui_dock::NodeIndex::root(), 0.5, vec![tab]);
        dock.set_focused_node_and_surface(egui_dock::NodePath { surface: egui_dock::SurfaceIndex::main(), node: new });
        return;
    }
    match dock.focused_leaf() {
        Some(path) => {
            dock.split(path, Split::Right, 0.5, Node::leaf_with(vec![tab]));
        }
        None => dock.push_to_focused_leaf(tab),
    }
}

/// Collected during rendering, applied by the app afterwards.
#[derive(Default)]
pub struct Viewer {
    pub inputs: Vec<(u64, Vec<u8>)>,
    pub reconnect: Vec<u64>,
    pub open_files: Vec<(u64, Vec<String>)>,
    pub open_folders: Vec<(u64, String)>,
    pub focused_uid: Option<u64>,
    /// Shells picked from a pane's "+" menu, opened in that pane.
    pub new_local: Vec<(egui_dock::NodePath, &'static ShellSpec)>,
    /// Agents picked from a pane's "+" menu (`agents_panel::launch_menu`).
    pub new_agent: Vec<(egui_dock::NodePath, super::agents_panel::LaunchPick)>,
    /// Frame time, for animated tab badges.
    pub time: f64,
    /// Pane behaviour for this frame (set by the app before showing the dock).
    pub panes: PaneFx,
    /// A tab the pointer rests on that should take focus (focus follows mouse).
    pub hover_focus: Option<u64>,
    /// Terminal widget ids seen this frame (the next frame's `PaneFx::terminal_ids`).
    pub terminal_ids: Vec<Id>,
    /// Picked from a dock tab's context menu (or a double-click on it: rename).
    pub tab_actions: Vec<TabAction>,
    /// Rendering the main window's dock (tab actions only apply there).
    pub in_main: bool,
    /// Agent tabs' title layouts this frame: where `on_tab_button` paints the agent
    /// icon and status pill.
    pub agent_titles: HashMap<u64, AgentTitle>,
    /// Detach / minimize requests from a terminal tab's linked right-hand SFTP pane.
    pub linked_sftp_actions: Vec<LinkedSftpAction>,
}

/// A dock tab title with room left for the painted agent icon and status pill.
pub struct AgentTitle {
    job: egui::text::LayoutJob,
    kind: AgentKind,
    /// Badge state and the char range of the (invisible) pill label in `job`.
    pill: Option<(AgentState, usize, usize)>,
}

/// Room before an agent tab's name for its icon.
const ICON_SLOT: f32 = 20.0;
const ICON_SIZE: f32 = 14.0;
/// Space between the name and the status pill.
const PILL_GAP: f32 = 7.0;
/// Width of egui_dock's close button (`Style::TAB_CLOSE_BUTTON_SIZE`).
const DOCK_CLOSE_W: f32 = 24.0;
/// The tab button under the pointer last frame (0: none): its usage chip shows.
static HOVERED_TAB: AtomicU64 = AtomicU64::new(0);

/// A close guard extends slightly into the label. Freeze the existing expansion
/// while crossing it, so both a compact and an expanded tab keep their X in place.
fn usage_hover(rect: egui::Rect, pointer: Option<egui::Pos2>, expanded: bool) -> bool {
    let Some(pos) = pointer.filter(|p| rect.contains(*p)) else { return false };
    if pos.x >= rect.right() - DOCK_CLOSE_W - 12.0 { expanded } else { true }
}

/// Per-frame inputs for pane dimming, outlines and focus handling.
#[derive(Default, Clone)]
pub struct PaneFx {
    /// The active tab of the focused pane.
    pub active_uid: Option<u64>,
    /// More than one pane is visible (dimming and outlines only make sense then).
    pub multi_pane: bool,
    /// 0 = off; how much unfocused terminal panes fade.
    pub dim: f32,
    /// Outline the focused pane (tab bars are hidden, so nothing else marks it).
    pub outline: bool,
    pub focus_follows_mouse: bool,
    /// Terminal widget ids of the previous frame: hover-focus only moves focus away
    /// from a terminal (or from nothing), never out of a text field.
    pub terminal_ids: Vec<Id>,
}

impl Viewer {
    pub fn with_panes(panes: PaneFx) -> Self {
        Self { panes, in_main: true, ..Self::default() }
    }

    /// Dim overlay, focus outline and focus-follows-mouse for a pane body.
    fn pane_effects(&mut self, ui: &mut Ui, tab: &Tab, body: egui::Rect) {
        let fx = &self.panes;
        let active = fx.active_uid == Some(tab.uid);
        let ctx = ui.ctx().clone();
        if fx.multi_pane && fx.dim > 0.0 && tab.is_terminal() {
            let t = ctx.animate_bool_with_time(Id::new(("pane_dim", tab.uid)), !active, theme::anim_time(0.18));
            if t > 0.0 {
                // Fade towards the terminal background: text recedes, the pane keeps its colour.
                let shade = theme::palette().term_bg.gamma_multiply(fx.dim * t);
                ui.painter().rect_filled(body, egui::CornerRadius::same(theme::RADIUS), shade);
            }
        }
        if fx.multi_pane && fx.outline {
            let t = ctx.animate_bool_with_time(Id::new(("pane_outline", tab.uid)), active, theme::anim_time(0.18));
            if t > 0.0 {
                let stroke = egui::Stroke::new(1.0, theme::accent().gamma_multiply(0.55 * t));
                ui.painter().rect_stroke(body.shrink(0.5), egui::CornerRadius::same(theme::RADIUS), stroke, egui::StrokeKind::Inside);
            }
        }
        if fx.focus_follows_mouse && !active && fx.multi_pane && ui.rect_contains_pointer(body) {
            let (moved, idle_buttons) = ui.input(|i| (i.pointer.delta() != egui::Vec2::ZERO, !i.pointer.any_down()));
            let free = ui.memory(|m| m.focused().is_none_or(|f| fx.terminal_ids.contains(&f)));
            if moved && idle_buttons && free && !egui::DragAndDrop::has_any_payload(&ctx) {
                self.hover_focus = Some(tab.uid);
            }
        }
    }
}

impl Viewer {
    /// Paints the agent icon and status pill into the room `agent_title` left in the
    /// tab button; a click on the pill opens its popover. True while the pointer is on
    /// the pill.
    fn agent_tab_button(&mut self, uid: u64, title: &AgentTitle, response: &egui::Response) -> bool {
        if response.dragged() {
            return false;
        }
        let ctx = &response.ctx;
        let mut job = title.job.clone();
        job.wrap.max_width = f32::INFINITY;
        let galley = ctx.fonts_mut(|f| f.layout_job(job));
        let Some(row) = galley.rows.first() else { return false };
        // egui_dock centres the title in the tab minus its close button.
        let tab = response.rect;
        let text_center = tab.left() + (tab.width() - DOCK_CLOSE_W) / 2.0;
        let origin = egui::pos2(text_center - galley.size().x / 2.0, tab.center().y - galley.size().y / 2.0);
        let glyph_x = |i: usize| row.row.glyphs.get(i).map(|g| origin.x + row.pos.x + g.pos.x);
        let painter = ctx.layer_painter(response.layer_id).with_clip_rect(tab);
        let icon = egui::Rect::from_center_size(egui::pos2(origin.x + (ICON_SLOT - 4.0) / 2.0, tab.center().y), egui::Vec2::splat(ICON_SIZE));
        agent_icons::paint(&painter, icon, title.kind, 1.0);
        let Some((state, start, end)) = title.pill else { return false };
        let (Some(label_x), Some(right)) = (glyph_x(start), glyph_x(end)) else { return false };
        let half = agent_status::PILL_H / 2.0;
        let pill = egui::Rect::from_min_max(egui::pos2(label_x - agent_status::LEAD, tab.center().y - half), egui::pos2(right, tab.center().y + half));
        let over = response.hovered() && ctx.pointer_hover_pos().is_some_and(|p| pill.contains(p));
        let hover = ctx.animate_bool_with_time(Id::new(("tab_pill", uid)), over, theme::anim_time(0.1));
        agent_status::paint(&painter, pill, state, self.time, hover);
        if response.clicked() && response.interact_pointer_pos().is_some_and(|p| pill.contains(p)) && self.in_main {
            agent_status::toggle_popover(ctx, uid, pill);
        }
        if over {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            response.clone().on_hover_text(agent_status::tooltip(title.kind, state));
        }
        over
    }

    /// With tab bars hidden (vertical tabs) the pane has no header: the status pill
    /// floats in its top-right corner instead.
    fn pane_status_pill(&mut self, ui: &mut Ui, uid: u64, body: egui::Rect) {
        if !self.in_main || !crate::settings::snapshot().layout.tab_bars_hidden() {
            return;
        }
        let Some(badge) = agent_badge::for_tab(uid) else { return };
        let w = agent_status::width(ui.painter(), badge.state, false);
        let rect = egui::Rect::from_min_size(egui::pos2(body.right() - w - 12.0, body.top() + 8.0), egui::vec2(w, agent_status::PILL_H));
        // A backing so the pill reads over terminal text.
        ui.painter().rect_filled(rect.expand(2.0), egui::CornerRadius::same(11), theme::palette().term_bg);
        let resp = agent_status::button(ui, rect, Id::new(("pane_pill", uid)), badge.kind, badge.state);
        if resp.clicked() {
            self.tab_actions.push(TabAction::Focus(uid));
            agent_status::toggle_popover(ui.ctx(), uid, rect);
        }
    }
}

/// The title of an agent tab: room for the icon, the name, then (badges on) the pill's
/// label laid out invisibly so the tab is wide enough, and the usage chip when high.
fn agent_title(name: &str, kind: AgentKind, state: Option<AgentState>, uid: u64) -> AgentTitle {
    use egui::text::{LayoutJob, TextFormat};
    let mut job = LayoutJob::default();
    let format =
        |size: f32, color: egui::Color32| TextFormat { font_id: egui::FontId::proportional(size), color, valign: egui::Align::Center, ..Default::default() };
    job.append(name, ICON_SLOT, format(12.5, egui::Color32::PLACEHOLDER));
    let mut pill = None;
    if let Some(state) = state
        && let Some(label) = agent_status::spec(state).label
    {
        let start = name.chars().count();
        job.append(label, PILL_GAP + agent_status::LEAD, format(agent_status::FONT_SIZE, egui::Color32::TRANSPARENT));
        let end = start + label.chars().count();
        job.append(" ", agent_status::TAIL, format(agent_status::FONT_SIZE, egui::Color32::TRANSPARENT));
        pill = Some((state, start, end));
    }
    usage_chip::append_to_title(&mut job, uid, HOVERED_TAB.load(Ordering::Relaxed) == uid);
    AgentTitle { job, kind, pill }
}

impl egui_dock::TabViewer for Viewer {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> Id {
        Id::new(("tab", tab.uid))
    }

    fn title(&mut self, tab: &mut Tab) -> WidgetText {
        let name: String = tab.display_title().chars().take(32).collect();
        let mut job = egui::text::LayoutJob::default();
        if let Some(agent) = tab.agent() {
            let title = agent_title(&name, agent.kind, agent_badge::for_tab(tab.uid).map(|b| b.state), tab.uid);
            job = title.job.clone();
            self.agent_titles.insert(tab.uid, title);
            return job.into();
        }
        let icon = tab.icon();
        agent_badge::append_title_mark(&mut job, tab.uid, tab.status_color(), self.time);
        let label = if tab.linked_sftp.is_some() { format!("{icon}  {name}  · 📁") } else { format!("{icon}  {name}") };
        RichText::new(label).size(12.5).append_to(&mut job, &egui::Style::default(), egui::FontSelection::Default, egui::Align::Center);
        if tab.is_local() {
            usage_chip::append_to_title(&mut job, tab.uid, HOVERED_TAB.load(Ordering::Relaxed) == tab.uid);
        }
        job.into()
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
        let body = ui.max_rect();
        // Fade + rise when a tab first appears.
        let now = ui.input(|i| i.time);
        let born = *tab.born.get_or_insert(now);
        let t = ((now - born) / APPEAR_SECS).clamp(0.0, 1.0) as f32;
        let eased = 1.0 - (1.0 - t).powi(3);
        if t < 1.0 {
            ui.ctx().request_repaint();
            ui.set_opacity(eased);
            ui.add_space((1.0 - eased) * 10.0);
        }
        let is_local = tab.is_local();
        let images_enabled = tab.agent().is_some_and(|a| matches!(a.kind, AgentKind::Claude | AgentKind::Codex));
        let tab_uid = tab.uid;
        let profile = tab.profile.clone();
        match &mut tab.body {
            TabBody::Terminal(view) => {
                view.images_enabled = images_enabled;
                if is_local {
                    if let Some(cwd) = crate::agents::hub::agent_cwd(tab_uid).or_else(|| crate::procmon::usage_for(tab_uid).and_then(|u| u.cwd.clone())) {
                        view.fallback_cwd = Some(cwd);
                    }
                } else {
                    view.fallback_cwd = None;
                }
                if let Some(linked) = &mut tab.linked_sftp {
                    let avail = ui.available_rect_before_wrap();
                    let sep_w = 6.0;
                    let left_w = ((avail.width() - sep_w) * linked.split_fraction).clamp(160.0, (avail.width() - sep_w - 180.0).max(160.0));
                    let term_rect = egui::Rect::from_min_max(avail.min, egui::pos2(avail.left() + left_w, avail.bottom()));
                    let sep_rect = egui::Rect::from_min_max(egui::pos2(term_rect.right(), avail.top()), egui::pos2(term_rect.right() + sep_w, avail.bottom()));
                    let sftp_rect = egui::Rect::from_min_max(egui::pos2(sep_rect.right(), avail.top()), avail.max);

                    // 1. Terminal on the left
                    let mut term_ui = ui.new_child(egui::UiBuilder::new().max_rect(term_rect).id_salt(("linked_term", tab_uid)));
                    term_ui.set_clip_rect(term_rect.intersect(ui.clip_rect()));
                    let term_id = term_ui.id().with("terminal");
                    let sftp_taking_focus = ui.rect_contains_pointer(sftp_rect)
                        && ui.input(|i| {
                            i.pointer.button_pressed(egui::PointerButton::Primary)
                                || i.pointer.button_pressed(egui::PointerButton::Secondary)
                                || i.events.iter().any(|e| {
                                    matches!(e, egui::Event::Paste(_))
                                        || matches!(e, egui::Event::Key { key: egui::Key::V, modifiers, .. } if modifiers.ctrl || modifiers.command)
                                })
                        });
                    if sftp_taking_focus && ui.memory(|m| m.has_focus(term_id)) {
                        ui.memory_mut(|m| m.surrender_focus(term_id));
                        linked.view.want_focus = true;
                    }
                    self.terminal_ids.push(term_id);
                    let out = view.ui(&mut term_ui, &profile);
                    if !out.input.is_empty() {
                        self.inputs.push((tab_uid, out.input));
                    }
                    for folder in out.open_folders {
                        self.open_folders.push((tab_uid, folder));
                    }
                    if !out.open_files.is_empty() {
                        self.open_files.push((tab_uid, out.open_files));
                    }
                    if out.reconnect {
                        self.reconnect.push(tab_uid);
                    }
                    if ui.memory(|m| m.focused().is_some_and(|f| f == term_id)) {
                        self.focused_uid = Some(tab_uid);
                    }
                    self.pane_status_pill(&mut term_ui, tab_uid, term_rect);

                    // 2. Resizable vertical splitter
                    let sep_id = ui.id().with(("linked_sftp_sep", tab_uid));
                    let sep_resp = ui.interact(sep_rect, sep_id, egui::Sense::drag());
                    if sep_resp.hovered() || sep_resp.dragged() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }
                    if sep_resp.dragged()
                        && let Some(pos) = sep_resp.interact_pointer_pos()
                    {
                        linked.split_fraction = ((pos.x - avail.left()) / avail.width().max(1.0)).clamp(0.2, 0.8);
                    }
                    let sep_col = if sep_resp.hovered() || sep_resp.dragged() { theme::accent() } else { theme::border().gamma_multiply(0.55) };
                    ui.painter().vline(sep_rect.center().x, sep_rect.y_range(), egui::Stroke::new(1.5, sep_col));

                    // 3. Linked SFTP on the right (with draggable header to separate/minimize/close)
                    let header_h = 26.0;
                    let header_rect = egui::Rect::from_min_max(sftp_rect.min, egui::pos2(sftp_rect.right(), sftp_rect.top() + header_h));
                    let sftp_body_rect = egui::Rect::from_min_max(egui::pos2(sftp_rect.left(), header_rect.bottom()), sftp_rect.max);

                    let mut close_linked = false;
                    let mut detach_linked = None;
                    let mut minimize_linked = false;

                    paint_linked_sftp_header(ui, tab_uid, &profile, header_rect, &mut close_linked, &mut detach_linked, &mut minimize_linked);

                    let mut sftp_ui = ui.new_child(egui::UiBuilder::new().max_rect(sftp_body_rect).id_salt(("linked_sftp", tab_uid)));
                    sftp_ui.set_clip_rect(sftp_body_rect.intersect(ui.clip_rect()));
                    let sftp_out = linked.view.ui(&mut sftp_ui);
                    if sftp_out.reconnect {
                        self.reconnect.push(tab_uid);
                    }
                    if sftp_out.close || close_linked {
                        tab.linked_sftp = None;
                    } else if minimize_linked {
                        self.linked_sftp_actions.push(LinkedSftpAction::Minimize { uid: tab_uid, rect: sftp_rect });
                    } else if let Some(pos) = detach_linked {
                        self.linked_sftp_actions.push(LinkedSftpAction::Detach { uid: tab_uid, pos, rect: sftp_rect });
                    }
                    ui.advance_cursor_after_rect(avail);
                } else {
                    let term_id = ui.id().with("terminal");
                    self.terminal_ids.push(term_id);
                    let out = view.ui(ui, &profile);
                    if !out.input.is_empty() {
                        self.inputs.push((tab_uid, out.input));
                    }
                    for folder in out.open_folders {
                        self.open_folders.push((tab_uid, folder));
                    }
                    if !out.open_files.is_empty() {
                        self.open_files.push((tab_uid, out.open_files));
                    }
                    if out.reconnect {
                        self.reconnect.push(tab_uid);
                    }
                    if ui.memory(|m| m.focused().is_some_and(|f| f == term_id)) {
                        self.focused_uid = Some(tab_uid);
                    }
                    self.pane_status_pill(ui, tab_uid, body);
                }
            }
            TabBody::Sftp(view) => {
                let out = view.ui(ui);
                if out.reconnect {
                    self.reconnect.push(tab.uid);
                }
                if out.close {
                    commands::push(AppCommand::CloseTab(tab.uid));
                }
            }
            TabBody::Editor(view) => {
                if view.ui(ui).close {
                    commands::push(AppCommand::CloseTab(tab.uid));
                }
            }
        }
        self.pane_effects(ui, tab, body);
    }

    fn on_close(&mut self, tab: &mut Tab) -> OnCloseResponse {
        match &mut tab.body {
            TabBody::Terminal(t) => t.handle.close(),
            // Unsaved changes: keep the tab and let the editor ask first.
            TabBody::Editor(e) => {
                if !e.request_close() {
                    return OnCloseResponse::Focus;
                }
            }
            TabBody::Sftp(_) => {}
        }
        OnCloseResponse::Close
    }

    fn on_tab_button(&mut self, tab: &mut Tab, response: &egui::Response) {
        let hovered = HOVERED_TAB.load(Ordering::Relaxed);
        let over = usage_hover(response.rect, response.ctx.pointer_hover_pos(), hovered == tab.uid);
        if over && hovered != tab.uid {
            HOVERED_TAB.store(tab.uid, Ordering::Relaxed);
            response.ctx.request_repaint();
        } else if !over && hovered == tab.uid {
            HOVERED_TAB.store(0, Ordering::Relaxed);
            response.ctx.request_repaint();
        }
        if let Some(title) = self.agent_titles.remove(&tab.uid)
            && self.agent_tab_button(tab.uid, &title, response)
        {
            // The pointer is on the status pill: its tooltip replaces the tab's.
            return;
        }
        // Where the shell is (shell integration / OSC 7), then the process tree's usage.
        if let TabBody::Terminal(t) = &tab.body
            && let Some((cwd, branch)) = t.shell_context()
        {
            response.clone().on_hover_text(context_line(cwd, branch));
        }
        if tab.is_local() {
            usage_chip::tab_tooltip(tab.uid, response);
        }
        if let TabBody::Editor(e) = &tab.body {
            response.clone().on_hover_text(format!("{}:{}", tab.profile.display_name(), e.remote_path()));
        }
        if self.in_main && response.double_clicked() {
            self.tab_actions.push(TabAction::StartRename(tab.uid));
        }
        if self.in_main && response.middle_clicked() {
            self.tab_actions.push(TabAction::Close(tab.uid));
        }
    }

    /// Added below egui_dock's own entries in a tab's right-click menu.
    fn context_menu(&mut self, ui: &mut Ui, tab: &mut Tab, _path: egui_dock::NodePath) {
        if !self.in_main {
            return;
        }
        if ui.button("✏  Rename…").clicked() {
            self.tab_actions.push(TabAction::StartRename(tab.uid));
            ui.close();
        }
        super::vertical_tabs::tab_menu_items(ui, tab.uid, &mut self.tab_actions);
    }

    /// The pane's "+" button: every detected local shell.
    fn add_popup(&mut self, ui: &mut Ui, path: egui_dock::NodePath) {
        ui.set_min_width(200.0);
        let Some(shells) = pty::shells::detected() else {
            ui.label(RichText::new(pty::shells::DETECTING).color(theme::text_dim()));
            return;
        };
        if shells.is_empty() {
            ui.label(RichText::new("No local shells found").color(theme::text_dim()));
        }
        for shell in shells {
            if ui.button(format!("{}  New {}", shell.icon_kind.glyph(), shell.name)).clicked() {
                self.new_local.push((path, shell));
                ui.close();
            }
        }
        if let Some(pick) = super::agents_panel::launch_menu(ui) {
            self.new_agent.push((path, pick));
        }
    }

    /// With a translucent window, terminals paint their own (translucent) background.
    fn clear_background(&self, tab: &Tab) -> bool {
        !(crate::themes::see_through() && matches!(tab.body, TabBody::Terminal(_)))
    }

    fn scroll_bars(&self, _tab: &Tab) -> [bool; 2] {
        [false, false]
    }
}

/// Compact draggable header bar for an SSH terminal's linked right-hand SFTP view.
/// Dragging the header away (or clicking Pop-out `🗗`) un-links the SFTP into its own
/// floating window; `—` minimizes it to the mini-bar; `✕` closes it.
fn paint_linked_sftp_header(
    ui: &mut Ui,
    tab_uid: u64,
    profile: &Profile,
    rect: egui::Rect,
    close_linked: &mut bool,
    detach_linked: &mut Option<Option<egui::Pos2>>,
    minimize_linked: &mut bool,
) {
    let p = ui.painter();
    p.rect_filled(rect, egui::CornerRadius::ZERO, theme::surface_2());
    p.hline(rect.x_range(), rect.bottom() - 0.5, egui::Stroke::new(1.0, theme::border().gamma_multiply(0.55)));

    let buttons_w = 72.0;
    let drag_rect = egui::Rect::from_min_max(rect.min, egui::pos2((rect.right() - buttons_w).max(rect.left() + 20.0), rect.bottom()));
    let drag_id = ui.id().with(("linked_sftp_hdr_drag", tab_uid));
    let drag_resp = ui.interact(drag_rect, drag_id, egui::Sense::click_and_drag()).on_hover_text("Linked SFTP · Drag to separate into an independent window");
    if drag_resp.hovered() || drag_resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    if drag_resp.dragged()
        && let Some(ptr) = ui.ctx().pointer_latest_pos()
        && !rect.expand(16.0).contains(ptr)
    {
        *detach_linked = Some(Some(ptr));
    }

    let tint = theme::hue_color(profile.hue);
    let label = format!("📁  SFTP · {}", profile.display_name());
    let galley = ui.painter().layout_no_wrap(label, egui::FontId::proportional(11.5), theme::text());
    ui.painter().circle_filled(egui::pos2(rect.left() + 10.0, rect.center().y), 3.0, tint);
    ui.painter().galley(egui::pos2(rect.left() + 18.0, rect.center().y - galley.size().y * 0.5), galley, theme::text());

    // Header buttons: Minimize (`—`), Separate/Pop-out (`🗗`), Close (`✕`)
    let btn_size = egui::vec2(20.0, 18.0);
    let y = rect.center().y - btn_size.y * 0.5;
    let min_rect = egui::Rect::from_min_size(egui::pos2(rect.right() - 68.0, y), btn_size);
    let pop_rect = egui::Rect::from_min_size(egui::pos2(rect.right() - 46.0, y), btn_size);
    let cls_rect = egui::Rect::from_min_size(egui::pos2(rect.right() - 24.0, y), btn_size);

    let min_resp = ui.interact(min_rect, drag_id.with("min"), egui::Sense::click()).on_hover_text("Minimize SFTP to mini-bar");
    if min_resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        ui.painter().rect_filled(min_rect, egui::CornerRadius::same(4), theme::accent_soft());
    }
    let mc = min_rect.center();
    let mcol = if min_resp.hovered() { theme::accent() } else { theme::text_dim() };
    ui.painter().line_segment([mc + egui::vec2(-4.0, 1.0), mc + egui::vec2(4.0, 1.0)], egui::Stroke::new(1.5, mcol));
    if min_resp.clicked() {
        *minimize_linked = true;
    }

    let pop_resp = ui.interact(pop_rect, drag_id.with("pop"), egui::Sense::click()).on_hover_text("Separate SFTP into floating window");
    if pop_resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        ui.painter().rect_filled(pop_rect, egui::CornerRadius::same(4), theme::surface_3());
    }
    let pcol = if pop_resp.hovered() { theme::text() } else { theme::text_dim() };
    ui.painter().rect_stroke(
        egui::Rect::from_center_size(pop_rect.center(), egui::vec2(9.0, 7.0)),
        egui::CornerRadius::same(2),
        egui::Stroke::new(1.3, pcol),
        egui::StrokeKind::Inside,
    );
    if pop_resp.clicked() {
        *detach_linked = Some(None);
    }

    let cls_resp = ui.interact(cls_rect, drag_id.with("cls"), egui::Sense::click()).on_hover_text("Close linked SFTP");
    if cls_resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        ui.painter().rect_filled(cls_rect, egui::CornerRadius::same(4), theme::danger().gamma_multiply(0.24));
    }
    let ccol = if cls_resp.hovered() { theme::danger() } else { theme::text_dim() };
    let cc = cls_rect.center();
    let d = 3.2;
    ui.painter().line_segment([cc + egui::vec2(-d, -d), cc + egui::vec2(d, d)], egui::Stroke::new(1.4, ccol));
    ui.painter().line_segment([cc + egui::vec2(d, -d), cc + egui::vec2(-d, d)], egui::Stroke::new(1.4, ccol));
    if cls_resp.clicked() {
        *close_linked = true;
    }
}

/// `C:\Users\me\src\tether` -> `~\src\tether`; long paths keep their last parts.
/// `~\src\app on main`: a folder and, inside a git repository, its branch. (The bundled
/// fonts have no branch glyph.)
pub fn context_line(folder: &str, branch: Option<&str>) -> String {
    match branch {
        Some(b) => format!("{folder} on {b}"),
        None => folder.to_string(),
    }
}

pub fn short_path(path: &str) -> String {
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_default();
    shorten_path(path, &home)
}

fn shorten_path(path: &str, home: &str) -> String {
    const KEEP: usize = 3;
    let trimmed = path.trim_end_matches(['/', '\\']);
    // Shells report `C:/Users/me` (OSC 7) for a `C:\Users\me` home: compare like with like.
    let home = home.trim_end_matches(['/', '\\']);
    let home_alt = if trimmed.contains('/') { home.replace('\\', "/") } else { home.to_string() };
    let home = home_alt.as_str();
    let shown = match trimmed.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with(['/', '\\'])) => format!("~{rest}"),
        _ => trimmed.to_string(),
    };
    if shown.is_empty() {
        return "/".into();
    }
    let sep = if shown.contains('\\') { '\\' } else { '/' };
    let parts: Vec<&str> = shown.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    if parts.len() > KEEP + 1 { format!("…{sep}{}", parts[parts.len() - KEEP..].join(&sep.to_string())) } else { shown }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_zone_freezes_both_compact_and_expanded_tabs() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(160.0, 28.0));
        for x in [125.0, 140.0, 158.0] {
            assert!(!usage_hover(rect, Some(egui::pos2(x, 14.0)), false));
            assert!(usage_hover(rect, Some(egui::pos2(x, 14.0)), true));
        }
        assert!(usage_hover(rect, Some(egui::pos2(50.0, 14.0)), false));
        assert!(!usage_hover(rect, Some(egui::pos2(50.0, 40.0)), true));
        assert!(!usage_hover(rect, None, true));
    }

    #[test]
    fn context_lines_add_the_branch() {
        assert_eq!(context_line("~/src/app", Some("main")), "~/src/app on main");
        assert_eq!(context_line("~/src/app", None), "~/src/app");
    }

    #[test]
    fn short_paths_use_tilde_and_keep_the_tail() {
        assert_eq!(shorten_path("/srv/app/", ""), "/srv/app");
        assert_eq!(shorten_path("/a/b/c/d/e", ""), "…/c/d/e");
        assert_eq!(shorten_path("/", ""), "/");
        assert_eq!(shorten_path(r"C:\Users\me\src", r"C:\Users\me"), r"~\src");
        assert_eq!(shorten_path(r"C:\Users\me", r"C:\Users\me\"), "~");
        assert_eq!(shorten_path(r"C:\Users\meow", r"C:\Users\me"), r"C:\Users\meow");
        assert_eq!(shorten_path("C:/Users/me/src", r"C:\Users\me"), "~/src");
    }
}
