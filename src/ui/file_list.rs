//! Virtualized file table shared by the local and remote panes: painted icons, Name / Type /
//! Size / Modified columns, hairline row separators, type-to-select and keyboard navigation.

use std::collections::BTreeSet;

use eframe::egui::{self, Align2, Color32, CornerRadius, Event, EventFilter, FontId, Key, Modifiers, Pos2, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};

use super::file_icons;
use super::type_select::{self, TypeAhead};
use crate::settings::{self, OpenWith};
use crate::theme;

pub const ROW_H: f32 = 30.0;
pub const HEADER_H: f32 = 26.0;
/// Horizontal inset of hover/selection backgrounds from the pane edges.
const ROW_INSET: f32 = 4.0;
const PAD: f32 = 12.0;
const ICON_CX: f32 = 22.0;
const NAME_X: f32 = 40.0;
const NAME_GAP: f32 = 10.0;
const TYPE_COL: f32 = 64.0;
const SIZE_COL: f32 = 80.0;
const DATE_COL: f32 = 128.0;
/// Below this pane width the date column is hidden so names stay readable.
const DATE_MIN_WIDTH: f32 = 470.0;
/// Below this pane width the type column is hidden too.
const TYPE_MIN_WIDTH: f32 = 380.0;
/// Rows moved by PageUp / PageDown.
const PAGE_ROWS: usize = 10;
const NAME_FONT: f32 = 13.0;
const META_FONT: f32 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Local,
    Remote,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub name: String,
    pub is_dir: bool,
    pub is_link: bool,
    pub size: u64,
    pub mtime: u64,
}

/// What is carried while dragging rows between panes.
#[derive(Debug, Clone)]
pub struct DragPayload {
    pub from: Side,
    pub names: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum ListAction {
    /// Enter a folder.
    Open(String),
    /// Double-click / Enter on a remote file: open it the way its type is configured.
    OpenFile(String),
    OpenInEditor(String),
    OpenWithSystem(String),
    /// Remember how files with this extension (lower-case, no dot; "" = none) open.
    AlwaysOpenWith(String, OpenWith),
    Transfer(Vec<String>),
    Copy(Vec<String>),
    Paste,
    Rename(String),
    Delete(Vec<String>),
    CopyPath(String),
    NewFolder,
    Refresh,
    Up,
}

#[derive(Default)]
pub struct ListState {
    pub selected: BTreeSet<String>,
    /// Start of a shift-range selection.
    anchor: Option<usize>,
    /// Row the keyboard moves from (last clicked / navigated row).
    cursor: Option<usize>,
    type_ahead: TypeAhead,
    /// Whether the list had keyboard focus last frame.
    has_focus: bool,
    /// Take keyboard focus on the next frame.
    want_focus: bool,
}

impl ListState {
    pub fn clear(&mut self) {
        self.selected.clear();
        self.anchor = None;
        self.cursor = None;
        self.type_ahead.reset();
    }

    /// True while the list owns keyboard focus (type-to-select and arrow keys go to it).
    pub fn has_focus(&self) -> bool {
        self.has_focus
    }

    pub fn request_focus(&mut self) {
        self.want_focus = true;
    }

    /// Current type-to-select prefix (empty when none is active).
    pub fn type_ahead(&self) -> &str {
        self.type_ahead.buffer()
    }

    fn select_only(&mut self, rows: &[Row], i: usize) {
        self.selected = BTreeSet::from([rows[i].name.clone()]);
        self.anchor = Some(i);
        self.cursor = Some(i);
    }
}

/// Horizontal layout of the columns, as offsets from the left edge of the list.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Columns {
    name_right: f32,
    type_x: Option<f32>,
    size_right: f32,
    date_right: Option<f32>,
}

impl Columns {
    fn new(width: f32) -> Self {
        let date_right = (width >= DATE_MIN_WIDTH).then_some(width - PAD);
        let size_right = width - PAD - if date_right.is_some() { DATE_COL } else { 0.0 };
        let type_x = (width >= TYPE_MIN_WIDTH).then_some(size_right - SIZE_COL - TYPE_COL);
        let name_right = type_x.unwrap_or(size_right - SIZE_COL) - NAME_GAP;
        Self { name_right: name_right.max(NAME_X + 40.0), type_x, size_right, date_right }
    }

    fn name_width(&self) -> f32 {
        self.name_right - NAME_X
    }
}

/// Splits a file name into the part drawn normally and the dimmed extension (with its dot).
/// Folders and names without a recognisable extension are not split.
fn split_name(name: &str, is_dir: bool) -> (&str, &str) {
    if is_dir || file_icons::extension(name).is_none() {
        return (name, "");
    }
    let dot = name.rfind('.').unwrap_or(name.len());
    name.split_at(dot)
}

pub fn show(ui: &mut Ui, side: Side, rows: &[Row], state: &mut ListState, transfer_label: &str) -> Vec<ListAction> {
    let mut actions = Vec::new();
    let list_id = ui.id().with("file_list");
    let whole = ui.available_rect_before_wrap();
    let cols = Columns::new(whole.width());
    header(ui, &cols);
    let body = ui.available_rect_before_wrap();

    // Background widget: owns keyboard focus and the empty-space menu. Registered before the
    // rows so the rows sit on top of it for hit testing.
    let bg = ui.interact(body, list_id, Sense::click());
    let clicked_inside = ui.rect_contains_pointer(whole)
        && ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary) || i.pointer.button_pressed(egui::PointerButton::Secondary))
        && !ui.ctx().any_popup_open();
    if std::mem::take(&mut state.want_focus) || clicked_inside {
        ui.memory_mut(|m| m.request_focus(list_id));
    }
    if state.cursor.is_some_and(|c| c >= rows.len()) {
        state.cursor = None;
        state.anchor = None;
    }

    let mut scroll_to = None;
    if keyboard_active(ui, list_id, whole, state.has_focus) && handle_keyboard(ui, side, rows, state, &mut actions, &mut scroll_to) {
        ui.memory_mut(|m| m.request_focus(list_id));
    }
    let has_focus = ui.memory(|m| m.has_focus(list_id));
    let mods = ui.input(|i| i.modifiers);
    let folder_color = if side == Side::Remote { theme::blue() } else { theme::warn() };

    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::vertical().id_salt((side as u8, "rows")).auto_shrink([false, false]).show_rows(ui, ROW_H, rows.len() + 1, |ui, range| {
            let content_top = ui.max_rect().top() - range.start as f32 * ROW_H;
            if let Some(i) = scroll_to {
                let top = content_top + (i + 1) as f32 * ROW_H;
                ui.scroll_to_rect(Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=top + ROW_H), None);
            }
            for idx in range {
                if idx == 0 {
                    if parent_row(ui) {
                        actions.push(ListAction::Up);
                    }
                    continue;
                }
                let i = idx - 1;
                let row = &rows[i];
                let selected = state.selected.contains(&row.name);
                let look = RowLook { selected, focused: has_focus, cursor: has_focus && state.cursor == Some(i), folder_color };
                let resp = draw_row(ui, row, &cols, look);

                if resp.clicked() || resp.secondary_clicked() || resp.drag_started() {
                    ui.memory_mut(|m| m.request_focus(list_id));
                }
                if resp.clicked() {
                    select(state, rows, i, mods);
                }
                if resp.double_clicked() {
                    actions.push(open_action(row, side));
                }
                if resp.drag_started() && !selected {
                    state.select_only(rows, i);
                }
                resp.dnd_set_drag_payload(DragPayload { from: side, names: state.selected.iter().cloned().collect() });
                if resp.dragged() {
                    drag_ghost(ui, state.selected.len().max(1));
                }
                resp.context_menu(|ui| row_menu(ui, side, row, i, rows, state, transfer_label, &mut actions));
            }
        });
    });

    bg.context_menu(|ui| {
        if ui.add(egui::Button::new("Paste").shortcut_text(crate::platform::shortcut("V"))).clicked() {
            actions.push(ListAction::Paste);
        }
        if ui.add(egui::Button::new("New folder").shortcut_text(if cfg!(target_os = "macos") { "⌘+Shift+N" } else { "F7" })).clicked() {
            actions.push(ListAction::NewFolder);
        }
        if ui.add(egui::Button::new("Refresh").shortcut_text(crate::keybindings::label(crate::keybindings::Action::SftpRefresh))).clicked() {
            actions.push(ListAction::Refresh);
        }
    });
    if bg.clicked() {
        state.clear();
        bg.request_focus();
    }
    let has_focus = ui.memory(|m| m.has_focus(list_id));
    if has_focus {
        let filter = EventFilter { horizontal_arrows: true, vertical_arrows: true, tab: false, escape: false };
        ui.memory_mut(|m| m.set_focus_lock_filter(list_id, filter));
    }
    state.has_focus = has_focus;
    type_ahead_pill(ui, body, &state.type_ahead);
    actions
}

/// Folders navigate; remote files open (editor / system app); local files upload.
fn open_action(row: &Row, side: Side) -> ListAction {
    match side {
        _ if row.is_dir => ListAction::Open(row.name.clone()),
        Side::Remote => ListAction::OpenFile(row.name.clone()),
        Side::Local => ListAction::Transfer(vec![row.name.clone()]),
    }
}

/// Lower-case extension used as the "open with" key ("" for names without one).
pub fn open_with_key(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn row_menu(ui: &mut Ui, side: Side, row: &Row, i: usize, rows: &[Row], state: &mut ListState, transfer_label: &str, actions: &mut Vec<ListAction>) {
    if !state.selected.contains(&row.name) {
        state.select_only(rows, i);
    }
    let names: Vec<String> = state.selected.iter().cloned().collect();
    if side == Side::Remote && !row.is_dir {
        open_menu(ui, &row.name, actions);
        ui.separator();
    }
    if ui.button(transfer_label).clicked() {
        actions.push(ListAction::Transfer(names.clone()));
    }
    if ui.add(egui::Button::new("Copy").shortcut_text(crate::platform::shortcut("C"))).clicked() {
        actions.push(ListAction::Copy(names.clone()));
    }
    if ui.add(egui::Button::new("Paste").shortcut_text(crate::platform::shortcut("V"))).clicked() {
        actions.push(ListAction::Paste);
    }
    if ui.add(egui::Button::new("Rename").shortcut_text(if cfg!(target_os = "macos") { "Return" } else { "F2" })).clicked() {
        actions.push(ListAction::Rename(row.name.clone()));
    }
    if ui.button("Copy path").clicked() {
        actions.push(ListAction::CopyPath(row.name.clone()));
    }
    ui.separator();
    if ui
        .add(egui::Button::new(egui::RichText::new("Delete").color(theme::danger())).shortcut_text(if cfg!(target_os = "macos") { "⌘+⌫" } else { "Del" }))
        .clicked()
    {
        actions.push(ListAction::Delete(names));
    }
}

/// "Open in editor", "Open with system app" and the per-extension default.
fn open_menu(ui: &mut Ui, name: &str, actions: &mut Vec<ListAction>) {
    let current = settings::get().open_with_for(name);
    let mark = |w: OpenWith| if current == w { "  (default)" } else { "" };
    if ui.button(format!("📝  Open in editor{}", mark(OpenWith::Editor))).clicked() {
        actions.push(ListAction::OpenInEditor(name.to_string()));
    }
    if ui.button(format!("🖥  Open with system app{}", mark(OpenWith::System))).clicked() {
        actions.push(ListAction::OpenWithSystem(name.to_string()));
    }
    ui.separator();
    let ext = open_with_key(name);
    let label = if ext.is_empty() { "Always open files without extension with".to_string() } else { format!("Always open .{ext} with") };
    ui.menu_button(label, |ui| {
        for (with, text) in [(OpenWith::Editor, "Editor"), (OpenWith::System, "System app")] {
            if ui.radio(current == with, text).clicked() {
                actions.push(ListAction::AlwaysOpenWith(ext.clone(), with));
                ui.close();
            }
        }
    });
}

/// The list takes typed keys while it has focus, or while the pointer is over it and no
/// other widget (text field, terminal) holds focus. Never under a modal or an open menu.
fn keyboard_active(ui: &Ui, list_id: egui::Id, rect: Rect, had_focus: bool) -> bool {
    let focused = ui.memory(|m| m.focused());
    let mine = focused == Some(list_id);
    let free = focused.is_none() || mine;
    let modal_ok = ui.memory(|m| m.top_modal_layer()).is_none_or(|l| l == ui.layer_id());
    free && (mine || had_focus || ui.rect_contains_pointer(rect)) && modal_ok && !ui.ctx().any_popup_open()
}

/// Applies navigation keys and typed characters. Returns true if anything was handled.
fn handle_keyboard(ui: &mut Ui, side: Side, rows: &[Row], state: &mut ListState, actions: &mut Vec<ListAction>, scroll_to: &mut Option<usize>) -> bool {
    let (keys, text, now) = ui.input_mut(|i| {
        let held = i.modifiers;
        let mut keys = Vec::new();
        let mut text = String::new();
        i.events.retain(|e| match e {
            Event::Key { key, pressed: true, modifiers, .. } if is_nav_key(*key, *modifiers) => {
                keys.push((*key, *modifiers));
                false
            }
            Event::Copy => {
                keys.push((Key::C, Modifiers::COMMAND));
                false
            }
            Event::Text(t) if !held.ctrl && !held.alt => {
                text.push_str(t);
                false
            }
            _ => true,
        });
        (keys, text, i.time)
    });
    let mut handled = false;
    let mut typed = false;
    for (key, mods) in keys {
        handled = true;
        if key == Key::Backspace && !mods.command {
            typed |= state.type_ahead.pop(now) && !state.type_ahead.buffer().is_empty();
        } else {
            apply_key(side, key, mods, rows, state, actions, scroll_to);
        }
    }
    for ch in text.chars() {
        typed |= state.type_ahead.push(ch, now);
    }
    if typed {
        handled = true;
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        let found = type_select::find(&names, state.type_ahead.buffer(), state.cursor);
        state.type_ahead.set_matched(found.is_some());
        if let Some(i) = found {
            state.select_only(rows, i);
            *scroll_to = Some(i);
        }
    }
    handled
}

fn is_nav_key(key: Key, mods: Modifiers) -> bool {
    let plain = !mods.ctrl && !mods.alt && !mods.command;
    let cmd = (mods.command || (!cfg!(target_os = "macos") && mods.ctrl)) && !mods.shift && !mods.alt;
    if cfg!(target_os = "macos") && mods.command && !mods.ctrl && !mods.alt {
        return (key == Key::Backspace && !mods.shift) || (key == Key::N && mods.shift) || (matches!(key, Key::A | Key::C | Key::ArrowDown) && !mods.shift);
    }
    match key {
        Key::ArrowUp | Key::ArrowDown | Key::Home | Key::End | Key::PageUp | Key::PageDown => plain,
        Key::Enter | Key::Backspace | Key::Delete | Key::F2 | Key::F7 => plain && !mods.shift,
        Key::A | Key::C => cmd,
        _ => false,
    }
}

fn apply_key(side: Side, key: Key, mods: Modifiers, rows: &[Row], state: &mut ListState, actions: &mut Vec<ListAction>, scroll_to: &mut Option<usize>) {
    if cfg!(target_os = "macos") && key == Key::ArrowDown && mods.command {
        open_selected(side, rows, state, actions);
        return;
    }
    if let Some(next) = nav_target(key, state.cursor, rows.len()) {
        let m = Modifiers { shift: mods.shift, ..Modifiers::NONE };
        select(state, rows, next, m);
        *scroll_to = Some(next);
        return;
    }
    match key {
        Key::Enter if cfg!(target_os = "macos") => {
            if let Some(row) = state.cursor.and_then(|c| rows.get(c)) {
                actions.push(ListAction::Rename(row.name.clone()));
            }
        }
        Key::Enter => open_selected(side, rows, state, actions),
        Key::Backspace if cfg!(target_os = "macos") && mods.command && !state.selected.is_empty() => {
            actions.push(ListAction::Delete(state.selected.iter().cloned().collect()));
        }
        Key::N if cfg!(target_os = "macos") && mods.command && mods.shift => actions.push(ListAction::NewFolder),
        Key::Backspace => {}
        Key::Delete if !state.selected.is_empty() => actions.push(ListAction::Delete(state.selected.iter().cloned().collect())),
        Key::C if !state.selected.is_empty() => actions.push(ListAction::Copy(state.selected.iter().cloned().collect())),
        Key::F2 => {
            if let Some(row) = state.cursor.and_then(|c| rows.get(c)) {
                actions.push(ListAction::Rename(row.name.clone()));
            }
        }
        Key::F7 => actions.push(ListAction::NewFolder),
        Key::A => {
            state.selected = rows.iter().map(|r| r.name.clone()).collect();
        }
        _ => {}
    }
}

fn open_selected(side: Side, rows: &[Row], state: &ListState, actions: &mut Vec<ListAction>) {
    if let Some(row) = state.cursor.and_then(|c| rows.get(c))
        && (row.is_dir || side == Side::Remote)
        && state.selected.len() <= 1
    {
        actions.push(open_action(row, side));
    } else if !state.selected.is_empty() {
        actions.push(ListAction::Transfer(state.selected.iter().cloned().collect()));
    }
}

/// Row the cursor moves to for a navigation key, or None for non-navigation keys / empty lists.
fn nav_target(key: Key, cursor: Option<usize>, len: usize) -> Option<usize> {
    let last = len.checked_sub(1)?;
    let next = match (key, cursor) {
        (Key::ArrowUp | Key::ArrowDown | Key::PageUp | Key::PageDown, None) => 0,
        (Key::ArrowDown, Some(c)) => (c + 1).min(last),
        (Key::ArrowUp, Some(c)) => c.saturating_sub(1),
        (Key::PageDown, Some(c)) => (c + PAGE_ROWS).min(last),
        (Key::PageUp, Some(c)) => c.saturating_sub(PAGE_ROWS),
        (Key::Home, _) => 0,
        (Key::End, _) => last,
        _ => return None,
    };
    Some(next)
}

fn select(state: &mut ListState, rows: &[Row], i: usize, mods: Modifiers) {
    let name = rows[i].name.clone();
    if mods.shift {
        let a = state.anchor.unwrap_or(i);
        let (lo, hi) = (a.min(i), a.max(i));
        if !(mods.command || (!cfg!(target_os = "macos") && mods.ctrl)) {
            state.selected.clear();
        }
        state.selected.extend(rows[lo..=hi].iter().map(|r| r.name.clone()));
        state.anchor = Some(a);
    } else if mods.command || (!cfg!(target_os = "macos") && mods.ctrl) {
        if !state.selected.remove(&name) {
            state.selected.insert(name);
        }
        state.anchor = Some(i);
    } else {
        state.selected = BTreeSet::from([name]);
        state.anchor = Some(i);
    }
    state.cursor = Some(i);
}

/// Small-caps style label: upper-case, faint, slightly letter-spaced.
fn caps_label(ui: &Ui, text: &str) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        text,
        0.0,
        egui::TextFormat { font_id: FontId::proportional(10.5), color: theme::text_faint(), extra_letter_spacing: 0.8, ..Default::default() },
    );
    ui.painter().layout_job(job)
}

fn header(ui: &mut Ui, cols: &Columns) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), HEADER_H), Sense::hover());
    let p = ui.painter();
    let y = rect.center().y;
    let left_label = |x: f32, text: &str| {
        let g = caps_label(ui, text);
        p.galley(pos2(rect.left() + x, y - g.size().y / 2.0), g, theme::text_faint());
    };
    let right_label = |x: f32, text: &str| {
        let g = caps_label(ui, text);
        p.galley(pos2(rect.left() + x - g.size().x, y - g.size().y / 2.0), g, theme::text_faint());
    };
    left_label(NAME_X, "NAME");
    if let Some(x) = cols.type_x {
        left_label(x, "TYPE");
    }
    right_label(cols.size_right, "SIZE");
    if let Some(x) = cols.date_right {
        right_label(x, "MODIFIED");
    }
    p.hline((rect.left() + ROW_INSET)..=(rect.right() - ROW_INSET), rect.bottom() - 0.5, Stroke::new(1.0, theme::border()));
}

/// Hover / selection background shared by the parent row and file rows.
fn row_background(ui: &Ui, rect: Rect, hover: f32, sel: f32, focused: bool) -> Rect {
    let bg = rect.shrink2(vec2(ROW_INSET, 1.5));
    let p = ui.painter();
    let radius = CornerRadius::same(6);
    // Hairline separator, hidden under highlighted rows.
    let sep_alpha = 1.0 - hover.max(sel);
    if sep_alpha > 0.0 {
        p.hline((rect.left() + NAME_X)..=(rect.right() - PAD), rect.bottom() - 0.5, Stroke::new(1.0, theme::row_separator().gamma_multiply(sep_alpha)));
    }
    if hover > 0.0 {
        p.rect_filled(bg, radius, theme::surface_2().gamma_multiply(hover));
    }
    if sel > 0.0 {
        let fill = if focused { theme::accent_soft() } else { theme::accent_soft().gamma_multiply(0.6) };
        p.rect_filled(bg, radius, fill.gamma_multiply(sel));
        let bar = Rect::from_min_size(pos2(bg.left(), bg.top() + 5.0), vec2(3.0, bg.height() - 10.0));
        p.rect_filled(bar, CornerRadius::same(2), theme::accent().gamma_multiply(sel * if focused { 1.0 } else { 0.6 }));
    }
    bg
}

fn parent_row(ui: &mut Ui) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
    let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    row_background(ui, rect, hover, 0.0, false);
    let p = ui.painter();
    file_icons::paint_parent(p, file_icons::icon_rect(pos2(rect.left() + ICON_CX, rect.center().y)), theme::text_dim());
    p.text(pos2(rect.left() + NAME_X, rect.center().y), Align2::LEFT_CENTER, "..", FontId::proportional(NAME_FONT), theme::text_dim());
    resp.on_hover_text("Parent folder (Alt+Up)").clicked()
}

#[derive(Clone, Copy)]
struct RowLook {
    selected: bool,
    focused: bool,
    cursor: bool,
    folder_color: Color32,
}

fn draw_row(ui: &mut Ui, row: &Row, cols: &Columns, look: RowLook) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click_and_drag());
    let ctx = ui.ctx();
    let hover = ctx.animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    let sel = ctx.animate_bool_with_time(resp.id.with("sel"), look.selected, 0.14);
    let bg = row_background(ui, rect, hover, sel, look.focused);
    let p = ui.painter();
    if look.cursor && !look.selected {
        p.rect_stroke(bg, CornerRadius::same(6), Stroke::new(1.0, theme::accent().gamma_multiply(0.4)), egui::StrokeKind::Inside);
    }
    let cy = rect.center().y;
    file_icons::paint_entry(p, pos2(rect.left() + ICON_CX, cy), &row.name, row.is_dir, row.is_link, look.folder_color);
    paint_name(ui, row, pos2(rect.left() + NAME_X, cy), cols.name_width());

    let p = ui.painter();
    let meta = FontId::proportional(META_FONT);
    if let Some(x) = cols.type_x {
        let label = file_icons::type_label(&row.name, row.is_dir, row.is_link);
        p.text(pos2(rect.left() + x, cy), Align2::LEFT_CENTER, label, FontId::proportional(11.5), theme::text_faint());
    }
    if !row.is_dir {
        p.text(pos2(rect.left() + cols.size_right, cy), Align2::RIGHT_CENTER, theme::human_bytes(row.size), meta.clone(), theme::text_dim());
    }
    if let Some(x) = cols.date_right {
        p.text(pos2(rect.left() + x, cy), Align2::RIGHT_CENTER, format_time(row.mtime), meta, theme::text_faint());
    }
    resp
}

/// Full file name with a dimmed extension. Long names are elided before the extension
/// ("very-long-na….png") so the type stays readable.
fn paint_name(ui: &Ui, row: &Row, left_center: Pos2, max_width: f32) {
    let font = FontId::proportional(NAME_FONT);
    let (stem, ext) = split_name(&row.name, row.is_dir);
    let stem_color = if row.is_dir { theme::text() } else { Color32::from_gray(215) };
    let p = ui.painter();
    let ext_galley = (!ext.is_empty()).then(|| p.layout_no_wrap(ext.to_string(), font.clone(), theme::text_faint()));
    let ext_w = ext_galley.as_ref().map_or(0.0, |g| g.size().x);
    let mut job = egui::text::LayoutJob::simple_singleline(stem.to_string(), font, stem_color);
    job.wrap = egui::text::TextWrapping { max_width: (max_width - ext_w).max(16.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let stem_galley = p.layout_job(job);
    let y = left_center.y - stem_galley.size().y / 2.0;
    let stem_w = stem_galley.size().x;
    p.galley(pos2(left_center.x, y), stem_galley, stem_color);
    if let Some(g) = ext_galley {
        p.galley(pos2(left_center.x + stem_w, y), g, theme::text_faint());
    }
}

/// Floating pill showing the type-to-select prefix; fades out after the search expires.
fn type_ahead_pill(ui: &Ui, body: Rect, ta: &TypeAhead) {
    let now = ui.input(|i| i.time);
    let alpha = ta.visibility(now);
    if alpha <= 0.0 {
        return;
    }
    ui.ctx().request_repaint();
    let p = ui.painter_at(body);
    let color = if ta.matched() { theme::text() } else { theme::danger() };
    let galley = p.layout_no_wrap(ta.buffer().to_string(), FontId::monospace(12.5), color);
    let size = vec2(galley.size().x + 38.0, 26.0);
    let rect = Rect::from_center_size(pos2(body.center().x, body.bottom() - 24.0), size);
    let radius = CornerRadius::same(13);
    let shadow = egui::epaint::Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha((110.0 * alpha) as u8) };
    p.add(shadow.as_shape(rect, radius));
    p.rect_filled(rect, radius, theme::surface_3().gamma_multiply(alpha));
    p.rect_stroke(rect, radius, Stroke::new(1.0, theme::accent().gamma_multiply(0.5 * alpha)), egui::StrokeKind::Inside);
    file_icons::paint_search(&p, pos2(rect.left() + 14.0, rect.center().y), theme::text_dim().gamma_multiply(alpha));
    let pos = pos2(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0);
    p.galley_with_override_text_color(pos, galley, color.gamma_multiply(alpha));
}

fn drag_ghost(ui: &Ui, count: usize) {
    let Some(pos) = ui.ctx().pointer_interact_pos() else { return };
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("drag_ghost")));
    let text = if count == 1 { "1 item".to_string() } else { format!("{count} items") };
    let galley = painter.layout_no_wrap(text, FontId::proportional(12.0), theme::text());
    let rect = Rect::from_min_size(pos + Vec2::new(14.0, 10.0), galley.size() + Vec2::new(20.0, 10.0));
    painter.rect_filled(rect, CornerRadius::same(12), theme::accent().gamma_multiply(0.9));
    painter.galley(rect.min + Vec2::new(10.0, 5.0), galley, theme::text());
}

/// `YYYY-MM-DD HH:MM` (UTC) without pulling in a date crate.
pub fn format_time(secs: u64) -> String {
    if secs == 0 {
        return String::new();
    }
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, (rem % 3600) / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_unix_time() {
        assert_eq!(format_time(0), "");
        assert_eq!(format_time(86_400), "1970-01-02 00:00");
        assert_eq!(format_time(1_700_000_000), "2023-11-14 22:13");
        assert_eq!(format_time(951_782_400), "2000-02-29 00:00");
    }

    fn rows() -> Vec<Row> {
        ["a", "b", "c", "d"].iter().map(|n| Row { name: n.to_string(), is_dir: false, is_link: false, size: 0, mtime: 0 }).collect()
    }

    #[test]
    fn shift_selects_range_ctrl_toggles() {
        let rows = rows();
        let mut s = ListState::default();
        select(&mut s, &rows, 1, Modifiers::NONE);
        select(&mut s, &rows, 3, Modifiers { shift: true, ..Default::default() });
        assert_eq!(s.selected.iter().cloned().collect::<Vec<_>>(), ["b", "c", "d"]);
        select(&mut s, &rows, 2, crate::keybindings::Binding::primary(Key::C, false, false).modifiers());
        assert_eq!(s.selected.iter().cloned().collect::<Vec<_>>(), ["b", "d"]);
    }

    #[test]
    fn shift_arrows_extend_from_anchor() {
        let rows = rows();
        let mut s = ListState::default();
        let mut actions = Vec::new();
        let mut scroll = None;
        apply_key(Side::Local, Key::ArrowDown, Modifiers::NONE, &rows, &mut s, &mut actions, &mut scroll);
        assert_eq!(s.cursor, Some(0));
        apply_key(Side::Local, Key::ArrowDown, Modifiers::SHIFT, &rows, &mut s, &mut actions, &mut scroll);
        apply_key(Side::Local, Key::ArrowDown, Modifiers::SHIFT, &rows, &mut s, &mut actions, &mut scroll);
        assert_eq!(s.selected.iter().cloned().collect::<Vec<_>>(), ["a", "b", "c"]);
        apply_key(Side::Local, Key::ArrowUp, Modifiers::SHIFT, &rows, &mut s, &mut actions, &mut scroll);
        assert_eq!(s.selected.iter().cloned().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(scroll, Some(1));
        assert!(actions.is_empty());
    }

    #[test]
    fn enter_backspace_delete_emit_actions() {
        let mut rows = rows();
        rows[0].is_dir = true;
        let mut s = ListState::default();
        let mut actions = Vec::new();
        let mut scroll = None;
        apply_key(Side::Local, Key::Home, Modifiers::NONE, &rows, &mut s, &mut actions, &mut scroll);
        apply_key(Side::Local, Key::Enter, Modifiers::NONE, &rows, &mut s, &mut actions, &mut scroll);
        apply_key(Side::Local, Key::End, Modifiers::NONE, &rows, &mut s, &mut actions, &mut scroll);
        apply_key(Side::Local, Key::Enter, Modifiers::NONE, &rows, &mut s, &mut actions, &mut scroll);
        apply_key(Side::Local, Key::Delete, Modifiers::NONE, &rows, &mut s, &mut actions, &mut scroll);
        apply_key(Side::Local, Key::Backspace, Modifiers::NONE, &rows, &mut s, &mut actions, &mut scroll);
        apply_key(Side::Local, Key::C, Modifiers::COMMAND, &rows, &mut s, &mut actions, &mut scroll);
        let shown: Vec<String> = actions.iter().map(|a| format!("{a:?}")).collect();
        let expected = if cfg!(target_os = "macos") {
            [r#"Rename("a")"#, r#"Rename("d")"#, r#"Delete(["d"])"#, r#"Copy(["d"])"#]
        } else {
            [r#"Open("a")"#, r#"Transfer(["d"])"#, r#"Delete(["d"])"#, r#"Copy(["d"])"#]
        };
        assert_eq!(shown, expected);
    }

    #[test]
    fn f2_renames_the_cursor_row_and_f7_makes_a_folder() {
        let rows = rows();
        let mut s = ListState::default();
        let mut actions = Vec::new();
        let mut scroll = None;
        apply_key(Side::Remote, Key::F2, Modifiers::NONE, &rows, &mut s, &mut actions, &mut scroll);
        assert!(actions.is_empty(), "nothing to rename without a cursor");
        apply_key(Side::Remote, Key::End, Modifiers::NONE, &rows, &mut s, &mut actions, &mut scroll);
        apply_key(Side::Remote, Key::F2, Modifiers::NONE, &rows, &mut s, &mut actions, &mut scroll);
        apply_key(Side::Remote, Key::F7, Modifiers::NONE, &rows, &mut s, &mut actions, &mut scroll);
        let shown: Vec<String> = actions.iter().map(|a| format!("{a:?}")).collect();
        assert_eq!(shown, [r#"Rename("d")"#, "NewFolder"]);
        assert!(is_nav_key(Key::F2, Modifiers::NONE) && !is_nav_key(Key::F2, Modifiers::CTRL));
    }

    #[test]
    fn remote_enter_opens_files_and_navigates_folders() {
        let mut rows = rows();
        rows[0].is_dir = true;
        let mut s = ListState::default();
        let mut actions = Vec::new();
        let mut scroll = None;
        apply_key(Side::Remote, Key::Home, Modifiers::NONE, &rows, &mut s, &mut actions, &mut scroll);
        apply_key(Side::Remote, Key::Enter, Modifiers::NONE, &rows, &mut s, &mut actions, &mut scroll);
        apply_key(Side::Remote, Key::End, Modifiers::NONE, &rows, &mut s, &mut actions, &mut scroll);
        apply_key(Side::Remote, Key::Enter, Modifiers::NONE, &rows, &mut s, &mut actions, &mut scroll);
        apply_key(Side::Remote, Key::ArrowUp, Modifiers::SHIFT, &rows, &mut s, &mut actions, &mut scroll);
        apply_key(Side::Remote, Key::Enter, Modifiers::NONE, &rows, &mut s, &mut actions, &mut scroll);
        let shown: Vec<String> = actions.iter().map(|a| format!("{a:?}")).collect();
        let expected = if cfg!(target_os = "macos") {
            [r#"Rename("a")"#, r#"Rename("d")"#, r#"Rename("c")"#]
        } else {
            [r#"Open("a")"#, r#"OpenFile("d")"#, r#"Transfer(["c", "d"])"#]
        };
        assert_eq!(shown, expected);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn finder_shortcuts_open_rename_create_and_request_confirmed_delete() {
        let rows = rows();
        let mut state = ListState::default();
        state.select_only(&rows, 1);
        let mut actions = Vec::new();
        let mut scroll = None;
        for (key, mods) in [
            (Key::ArrowDown, Modifiers::MAC_CMD | Modifiers::COMMAND),
            (Key::Enter, Modifiers::NONE),
            (Key::Backspace, Modifiers::MAC_CMD | Modifiers::COMMAND),
            (Key::N, Modifiers::MAC_CMD | Modifiers::COMMAND | Modifiers::SHIFT),
        ] {
            assert!(is_nav_key(key, mods));
            apply_key(Side::Remote, key, mods, &rows, &mut state, &mut actions, &mut scroll);
        }
        let shown: Vec<_> = actions.iter().map(|a| format!("{a:?}")).collect();
        assert_eq!(shown, [r#"OpenFile("b")"#, r#"Rename("b")"#, r#"Delete(["b"])"#, "NewFolder"]);
        let new_folder = Modifiers::MAC_CMD | Modifiers::COMMAND | Modifiers::SHIFT;
        assert!(
            crate::keybindings::Action::ALL.into_iter().all(|a| a.default_binding().is_none_or(|b| !b.matches(Key::N, new_folder))),
            "global shortcuts must not steal Finder's new-folder gesture"
        );
        assert!(!is_nav_key(Key::C, Modifiers::CTRL));
        assert!(!is_nav_key(Key::Backspace, Modifiers::MAC_CMD | Modifiers::SHIFT));
    }

    #[test]
    fn double_click_depends_on_side() {
        let file = Row { name: "x.conf".into(), is_dir: false, is_link: false, size: 0, mtime: 0 };
        assert!(matches!(open_action(&file, Side::Remote), ListAction::OpenFile(n) if n == "x.conf"));
        assert!(matches!(open_action(&file, Side::Local), ListAction::Transfer(_)));
        assert_eq!(open_with_key("A.PNG"), "png");
        assert_eq!(open_with_key("Makefile"), "");
    }

    #[test]
    fn nav_targets_clamp() {
        assert_eq!(nav_target(Key::ArrowDown, None, 3), Some(0));
        assert_eq!(nav_target(Key::ArrowDown, Some(2), 3), Some(2));
        assert_eq!(nav_target(Key::ArrowUp, Some(0), 3), Some(0));
        assert_eq!(nav_target(Key::PageDown, Some(1), 30), Some(11));
        assert_eq!(nav_target(Key::End, None, 3), Some(2));
        assert_eq!(nav_target(Key::Home, Some(2), 0), None);
        assert_eq!(nav_target(Key::Enter, Some(1), 3), None);
    }

    #[test]
    fn splits_extension_for_dimming() {
        assert_eq!(split_name("photo.png", false), ("photo", ".png"));
        assert_eq!(split_name("backup.tar.gz", false), ("backup.tar", ".gz"));
        assert_eq!(split_name(".bashrc", false), (".bashrc", ""));
        assert_eq!(split_name("Makefile", false), ("Makefile", ""));
        assert_eq!(split_name("dir.d", true), ("dir.d", ""));
    }

    #[test]
    fn columns_hide_progressively() {
        let wide = Columns::new(600.0);
        assert!(wide.type_x.is_some() && wide.date_right.is_some());
        let mid = Columns::new(420.0);
        assert!(mid.type_x.is_some() && mid.date_right.is_none());
        let narrow = Columns::new(300.0);
        assert!(narrow.type_x.is_none() && narrow.date_right.is_none());
        assert_eq!(narrow.size_right, 300.0 - PAD);
        for c in [wide, mid, narrow] {
            assert!(c.name_width() >= 40.0);
            assert!(c.name_right <= c.type_x.unwrap_or(c.size_right - SIZE_COL));
        }
    }
}
