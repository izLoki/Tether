//! Every sidebar menu (right-click, "⋯" and the header's "New" dropdown) in one place,
//! so a host or folder offers the same actions however the menu was opened.

use eframe::egui::{self, AtomExt, RichText, Ui, Vec2};
use uuid::Uuid;

use super::{OpenKind, Sidebar, SidebarAction, SidebarData, count_hosts};
use crate::folders::Folder;
use crate::keybindings::{self, Action};
use crate::profile::Profile;
use crate::pty::ShellSpec;
use crate::theme;

pub(super) const MENU_W: f32 = 236.0;
const ITEM_H: f32 = 26.0;

/// A full-width menu row: fixed icon column, label, right-aligned hint.
fn row<'a>(ui: &mut Ui, icon: &'a str, label: &'a str, hint: Option<String>, color: egui::Color32) -> egui::Response {
    let icon = RichText::new(icon).size(13.0).color(color.gamma_multiply(0.85)).atom_size(Vec2::new(18.0, 14.0));
    let mut button =
        egui::Button::new((icon, RichText::new(label).size(13.0).color(color))).frame_when_inactive(false).min_size(Vec2::new(ui.available_width(), ITEM_H));
    if let Some(hint) = hint.filter(|h| !h.is_empty()) {
        button = button.shortcut_text(RichText::new(hint).size(11.0).color(theme::text_faint()));
    }
    let resp = ui.add(button);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

pub(super) fn item(ui: &mut Ui, icon: &str, label: &str) -> egui::Response {
    row(ui, icon, label, None, theme::text())
}

fn hinted(ui: &mut Ui, icon: &str, label: &str, hint: impl Into<String>) -> egui::Response {
    row(ui, icon, label, Some(hint.into()), theme::text())
}

fn bound(ui: &mut Ui, icon: &str, label: &str, action: Action) -> egui::Response {
    row(ui, icon, label, Some(keybindings::label(action)), theme::text())
}

pub(super) fn danger(ui: &mut Ui, icon: &str, label: &str) -> egui::Response {
    row(ui, icon, label, None, theme::danger())
}

/// Small caps title at the top of a menu ("FOLDER · Production").
pub(super) fn title(ui: &mut Ui, kind: &str, name: &str) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        ui.label(RichText::new(kind).size(10.0).color(theme::text_faint()));
        ui.add(egui::Label::new(RichText::new(name).size(11.5).color(theme::text_dim())).truncate());
    });
    ui.add_space(2.0);
    ui.separator();
}

pub(super) fn begin(ui: &mut Ui) {
    ui.set_width(MENU_W);
    ui.spacing_mut().item_spacing.y = 1.0;
    ui.spacing_mut().button_padding = Vec2::new(8.0, 4.0);
}

impl Sidebar {
    /// Right-click on empty space or the HOSTS header.
    pub(super) fn background_menu(&mut self, ui: &mut Ui, actions: &mut Vec<SidebarAction>) {
        begin(ui);
        if bound(ui, "+", "New host", Action::NewHost).clicked() {
            actions.push(SidebarAction::New(None));
        }
        if bound(ui, "📁", "New folder", Action::NewFolder).clicked() {
            actions.push(SidebarAction::NewFolder(None));
        }
        ui.separator();
        if bound(ui, "⚡", "Quick connect", Action::QuickConnect).clicked() {
            self.focus_quick = true;
        }
        if bound(ui, "🔍", "Search hosts", Action::SearchHosts).clicked() {
            self.focus_search = true;
        }
        ui.separator();
        expand_collapse(ui, actions);
        ui.separator();
        if item(ui, "⬇", "Import / export setup…").clicked() {
            actions.push(SidebarAction::Import);
        }
        if bound(ui, "🔑", "Key manager", Action::KeyManager).clicked() {
            actions.push(SidebarAction::OpenKeys);
        }
    }

    /// The header's "New" dropdown.
    pub(super) fn new_menu(&mut self, ui: &mut Ui, shells: &[ShellSpec], actions: &mut Vec<SidebarAction>) {
        begin(ui);
        // The default shell carries the shortcut; the rest follow in detection order.
        let default = crate::pty::shells::pick_default(shells, &crate::settings::get().terminal.default_shell).map(|s| s.id.clone());
        for shell in shells {
            let label = format!("New {}", shell.name);
            let resp = if Some(&shell.id) == default.as_ref() {
                bound(ui, shell.icon_kind.glyph(), &label, Action::NewLocalTerminal)
            } else {
                item(ui, shell.icon_kind.glyph(), &label)
            };
            if resp.clicked() {
                actions.push(SidebarAction::NewLocal(shell.id.clone()));
            }
        }
        if crate::pty::shells::detected().is_none() {
            ui.label(egui::RichText::new(crate::pty::shells::DETECTING).color(crate::theme::text_dim()));
        }
        if !shells.is_empty() || crate::pty::shells::detected().is_none() {
            ui.separator();
        }
        if bound(ui, "+", "New host", Action::NewHost).clicked() {
            actions.push(SidebarAction::New(None));
        }
        if bound(ui, "📁", "New folder", Action::NewFolder).clicked() {
            actions.push(SidebarAction::NewFolder(None));
        }
        ui.separator();
        if bound(ui, "⚡", "Quick connect", Action::QuickConnect).clicked() {
            self.focus_quick = true;
        }
        if item(ui, "⬇", "Import / export setup…").clicked() {
            actions.push(SidebarAction::Import);
        }
    }

    pub(super) fn folder_menu(&mut self, ui: &mut Ui, f: &Folder, data: &SidebarData, actions: &mut Vec<SidebarAction>) {
        begin(ui);
        title(ui, "FOLDER", &f.name);
        if item(ui, "+", "New host here").clicked() {
            actions.push(SidebarAction::New(Some(f.id)));
        }
        if item(ui, "📁", "New subfolder").clicked() {
            actions.push(SidebarAction::NewFolder(Some(f.id)));
        }
        ui.separator();
        if hinted(ui, "✏", "Rename", "F2").clicked() {
            self.renaming = Some((f.id, f.name.clone()));
        }
        ui.menu_button((RichText::new("📂").size(13.0).atom_size(Vec2::new(18.0, 14.0)), RichText::new("Move to").size(13.0)), |ui| {
            begin(ui);
            let blocked = data.folders.descendants(f.id);
            move_targets(ui, data, f.parent, |id| id != f.id && !blocked.contains(&id), |target| actions.push(SidebarAction::MoveFolder(f.id, target)));
        });
        ui.separator();
        expand_collapse(ui, actions);
        let count = count_hosts(data, f.id);
        if count > 0
            && hinted(ui, "📥", "Archive its hosts", count.to_string()).on_hover_text("Moves every host in this folder (and below) to Archived").clicked()
        {
            actions.push(SidebarAction::ArchiveFolder(f.id));
        }
        ui.separator();
        if danger(ui, "🗑", "Delete folder").on_hover_text("Its hosts and sub-folders move up a level").clicked() {
            actions.push(SidebarAction::DeleteFolder(f.id));
        }
    }
}

fn expand_collapse(ui: &mut Ui, actions: &mut Vec<SidebarAction>) {
    if item(ui, "⊞", "Expand all folders").clicked() {
        actions.push(SidebarAction::CollapseAll(false));
    }
    if item(ui, "⊟", "Collapse all folders").clicked() {
        actions.push(SidebarAction::CollapseAll(true));
    }
}

/// "Top level" plus the folder tree, indented; `current` and rejected folders are greyed out.
fn move_targets(ui: &mut Ui, data: &SidebarData, current: Option<Uuid>, allowed: impl Fn(Uuid) -> bool, mut pick: impl FnMut(Option<Uuid>)) {
    if ui
        .add_enabled(current.is_some(), egui::Button::new("⬆  Top level").frame_when_inactive(false).min_size(Vec2::new(ui.available_width(), ITEM_H)))
        .clicked()
    {
        pick(None);
    }
    let tree = data.folders.flattened();
    if !tree.is_empty() {
        ui.separator();
    }
    for (id, depth) in tree {
        let name = data.folders.get(id).map(|f| f.name.as_str()).unwrap_or_default();
        let label = format!("{}📁  {name}", "    ".repeat(depth));
        let enabled = current != Some(id) && allowed(id);
        if ui.add_enabled(enabled, egui::Button::new(label).frame_when_inactive(false).min_size(Vec2::new(ui.available_width(), ITEM_H))).clicked() {
            pick(Some(id));
        }
    }
}

pub(super) fn host_menu(ui: &mut Ui, p: &Profile, data: &SidebarData, actions: &mut Vec<SidebarAction>) {
    begin(ui);
    title(ui, "HOST", p.display_name());
    if hinted(ui, "⌨", "Connect terminal", "Double-click").clicked() {
        actions.push(SidebarAction::Open(p.id, OpenKind::Terminal));
    }
    if item(ui, "📁", "Open SFTP").clicked() {
        actions.push(SidebarAction::Open(p.id, OpenKind::Sftp));
    }
    if item(ui, "🗗", "Terminal + SFTP").clicked() {
        actions.push(SidebarAction::Open(p.id, OpenKind::Both));
    }
    ui.separator();
    if item(ui, "✏", "Edit…").clicked() {
        actions.push(SidebarAction::Edit(p.id));
    }
    if item(ui, "🗐", "Duplicate").clicked() {
        actions.push(SidebarAction::Duplicate(p.id));
    }
    ui.menu_button((RichText::new("📂").size(13.0).atom_size(Vec2::new(18.0, 14.0)), RichText::new("Move to").size(13.0)), |ui| {
        begin(ui);
        move_targets(ui, data, p.folder, |_| true, |target| actions.push(SidebarAction::MoveHost(p.id, target)));
    });
    let (icon, label, archive) = if p.archived { ("📤", "Unarchive", false) } else { ("📥", "Archive", true) };
    if item(ui, icon, label).clicked() {
        actions.push(SidebarAction::Archive(p.id, archive));
    }
    ui.separator();
    if item(ui, "🔑", "Forget host key").on_hover_text("Use after the server was reinstalled; the next connect re-learns its key").clicked() {
        actions.push(SidebarAction::ForgetHostKey(p.id));
    }
    if danger(ui, "🗑", "Delete host…").clicked() {
        actions.push(SidebarAction::Delete(p.id));
    }
}
