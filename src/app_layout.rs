//! Tab and pane operations (focus, move, split, zoom, rename) and the small layout
//! commands (toggle containers, tab position). The view containers themselves are in
//! `app_views.rs`. A child module of `app`.

use eframe::egui;
use egui_dock::{DockState, Node, NodePath, TabIndex, TabInsert, TabViewer};

use super::TetherApp;
use crate::keybindings::{self, Action};
use crate::layout::{Container, Direction, Side, TabPosition, View};
use crate::profile::secret;
use crate::settings;
use crate::theme;
use crate::ui::settings_view::{Section, SettingsView};
use crate::ui::tabs::{PaneFx, Tab, TabBody, Viewer};
use crate::ui::vertical_tabs::{TabAction, TabTarget};
use crate::workspace::TabKind;

impl TetherApp {
    // ---- tab operations -------------------------------------------------------------

    pub(super) fn apply_tab_actions(&mut self, actions: Vec<TabAction>, ctx: &egui::Context) {
        for a in actions {
            match a {
                TabAction::Focus(uid) => {
                    if self.mini_dock.restore_by_uid(ctx, &mut self.dock, uid) || !self.detached.focus_tab(ctx, uid) {
                        self.show_tab(uid);
                    }
                }
                TabAction::Close(uid) => self.close_tab(uid),
                TabAction::CloseOthers(uid) => {
                    let others: Vec<u64> = self.dock.iter_all_tabs().filter(|(p, t)| p.surface.is_main() && t.uid != uid).map(|(_, t)| t.uid).collect();
                    for other in others {
                        self.close_tab(other);
                    }
                    self.show_tab(uid);
                }
                TabAction::StartRename(uid) => {
                    if let Some(tab) =
                        self.dock.iter_all_tabs_mut().map(|(_, t)| t).chain(self.detached.tabs_mut()).chain(self.mini_dock.tabs_mut()).find(|t| t.uid == uid)
                    {
                        self.rename = Some((uid, tab.display_title()));
                    }
                }
                TabAction::Rename(uid, name) => {
                    if let Some(tab) =
                        self.dock.iter_all_tabs_mut().map(|(_, t)| t).chain(self.detached.tabs_mut()).chain(self.mini_dock.tabs_mut()).find(|t| t.uid == uid)
                    {
                        tab.rename(name);
                    }
                    // Custom titles are part of the saved layout.
                    self.last_layout.clear();
                }
                TabAction::Duplicate(uid) => {
                    if let (Some(tab), Some(path)) = (self.duplicate_of(uid, ctx), self.dock.find_tab_from(|t| t.uid == uid)) {
                        let new_uid = tab.uid;
                        if let Ok(leaf) = self.dock.leaf_mut(path.node_path()) {
                            let idx = (path.tab.0 + 1).min(leaf.tabs.len());
                            leaf.insert_tab(TabIndex(idx), tab);
                        }
                        self.show_tab(new_uid);
                    }
                }
                TabAction::SplitWith(uid, split) => {
                    if let (Some(tab), Some(path)) = (self.duplicate_of(uid, ctx), self.dock.find_tab_from(|t| t.uid == uid)) {
                        let new_uid = tab.uid;
                        self.dock.split(path.node_path(), split, 0.5, Node::leaf_with(vec![tab]));
                        self.show_tab(new_uid);
                    }
                }
                TabAction::PopOut(uid) => {
                    if let Some(path) = self.dock.find_tab_from(|t| t.uid == uid)
                        && let Some(tab) = self.dock.remove_tab(path)
                    {
                        let rect = crate::ui::mini_dock::default_floating_rect(ctx.content_rect());
                        let surf = self.dock.add_window(vec![tab]);
                        if let Some(state) = self.dock.get_window_state_mut(surf) {
                            state.set_position(rect.min).set_size(rect.size());
                        }
                    }
                }
                TabAction::MinimizeWindow(uid) => {
                    let fallback_rect = crate::ui::mini_dock::default_floating_rect(ctx.content_rect());
                    if let Some(path) = self.dock.find_tab_from(|t| t.uid == uid) {
                        if !path.surface.is_main() {
                            let area_id = egui::Id::new(format!("window {:?}", path.surface));
                            let rect = ctx.memory(|m| m.area_rect(area_id)).unwrap_or(fallback_rect);
                            self.mini_dock.minimize_surface(ctx, &mut self.dock, path.surface, rect);
                        } else if let Some(tab) = self.dock.remove_tab(path) {
                            self.mini_dock.minimize_tabs(ctx, vec![tab], fallback_rect);
                        }
                    } else if let Some(tab) = self.detached.remove_tab_by_uid(uid) {
                        self.mini_dock.minimize_tabs(ctx, vec![tab], fallback_rect);
                    }
                }
                TabAction::Move { uid, target } => self.move_tab(uid, target),
                TabAction::NewShell(id) => {
                    let spec = id.as_deref().and_then(crate::pty::shells::find);
                    self.open_local(spec, ctx);
                }
                TabAction::SetGrouped(on) => {
                    let _ = settings::update(|s| s.layout.group_by_pane = on);
                }
                TabAction::SetCompact(on) => {
                    let _ = settings::update(|s| s.layout.compact_tabs = on);
                }
                TabAction::SetPosition(p) => set_tab_position(p),
            }
        }
    }

    /// Shows `uid` in its pane, focuses that pane and gives its terminal the keyboard.
    pub(super) fn show_tab(&mut self, uid: u64) {
        let Some(path) = self.dock.find_tab_from(|t| t.uid == uid) else { return };
        let _ = self.dock.set_active_tab(path);
        self.dock.set_focused_node_and_surface(path.node_path());
        self.focus_request = Some(uid);
        if self.zoomed.is_some() {
            self.zoomed = Some(uid);
        }
    }

    /// Closes a tab of the main dock, a floating window, a detached window, or a minimized
    /// window the way its ✕ would (editors may refuse).
    pub(super) fn close_tab(&mut self, uid: u64) {
        if let Some(path) = self.dock.find_tab_from(|t| t.uid == uid) {
            let Some(tab) = self.dock.iter_all_tabs_mut().map(|(_, t)| t).find(|t| t.uid == uid) else { return };
            let close = match &mut tab.body {
                TabBody::Terminal(t) => {
                    t.handle.close();
                    true
                }
                TabBody::Editor(e) => e.request_close(),
                TabBody::Sftp(_) => true,
            };
            if close {
                self.dock.remove_tab(path);
            }
            return;
        }
        if let Some(mut tab) = self.detached.remove_tab_by_uid(uid) {
            if let TabBody::Terminal(t) = &mut tab.body {
                t.handle.close();
            }
            return;
        }
        if let Some(mut tab) = self.mini_dock.remove_tab_by_uid(uid)
            && let TabBody::Terminal(t) = &mut tab.body
        {
            t.handle.close();
        }
    }

    /// A new tab running the same thing as `uid` (local shells start in its folder).
    fn duplicate_of(&mut self, uid: u64, ctx: &egui::Context) -> Option<Tab> {
        let tab = self.dock.iter_all_tabs().map(|(_, t)| t).find(|t| t.uid == uid)?;
        if let Some((spec, cwd)) = super::local::origin(tab) {
            return Some(self.local_tab(&spec, cwd, ctx));
        }
        let kind = match tab.body {
            TabBody::Terminal(_) => TabKind::Terminal,
            TabBody::Sftp(_) => TabKind::Sftp,
            TabBody::Editor(_) => return None,
        };
        let profile = self.store.get(tab.profile.id).cloned().unwrap_or_else(|| tab.profile.clone());
        let secret = secret::get(profile.secret_id());
        Some(match kind {
            TabKind::Sftp => self.sftp_tab(&profile, secret, ctx),
            _ => self.terminal_tab(&profile, secret, ctx),
        })
    }

    fn move_tab(&mut self, uid: u64, target: TabTarget) {
        let Some(src) = self.dock.find_tab_from(|t| t.uid == uid) else { return };
        let same_leaf = |leaf: NodePath| src.node_path() == leaf;
        match target {
            TabTarget::Insert { leaf, index } => {
                // `index` counts the dragged tab when it sits before the drop point.
                let index = if same_leaf(leaf) && src.tab.0 < index { index - 1 } else { index };
                if same_leaf(leaf) && src.tab.0 == index {
                    return;
                }
                if self.dock.leaf(leaf).is_err() {
                    return;
                }
                self.dock.move_tab(src, (leaf, TabInsert::Insert(TabIndex(index))));
            }
            TabTarget::Append(leaf) => {
                if same_leaf(leaf) || self.dock.leaf(leaf).is_err() {
                    return;
                }
                self.dock.move_tab(src, (leaf, TabInsert::Append));
            }
            TabTarget::Split { leaf, split } => {
                if self.dock.leaf(leaf).is_err() {
                    return;
                }
                self.dock.move_tab(src, (leaf, TabInsert::Split(split)));
            }
        }
        self.show_tab(uid);
    }

    // ---- panes ---------------------------------------------------------------------

    /// Main-surface leaves that hold tabs, with their rects (as of the last frame).
    fn panes(&self) -> Vec<(NodePath, egui::Rect)> {
        self.dock.iter_leaves().filter(|(p, l)| p.surface.is_main() && !l.tabs.is_empty()).map(|(p, l)| (p, l.rect)).collect()
    }

    pub(super) fn focus_pane(&mut self, dir: Direction) {
        let panes = self.panes();
        let Some(cur) = self.dock.focused_leaf().filter(|p| p.surface.is_main()).or_else(|| panes.first().map(|p| p.0)) else { return };
        let Some(cur_rect) = panes.iter().find(|p| p.0 == cur).map(|p| p.1) else { return };
        let rects: Vec<egui::Rect> = panes.iter().map(|p| p.1).collect();
        let Some(i) = crate::layout::neighbor(cur_rect, &rects, dir) else { return };
        let target = panes[i].0;
        self.dock.set_focused_node_and_surface(target);
        if let Ok(leaf) = self.dock.leaf(target) {
            self.focus_request = leaf.tabs.get(leaf.active.0).map(|t| t.uid);
        }
    }

    pub(super) fn toggle_zoom(&mut self) {
        if self.zoomed.take().is_some() {
            if let Some(uid) = self.dock.find_active_focused().map(|(_, t)| t.uid) {
                self.focus_request = Some(uid);
            }
            return;
        }
        if self.panes().len() < 2 {
            self.toasts.info("Maximize needs a split: there is only one pane");
            return;
        }
        self.zoomed = self.dock.find_active_focused().map(|(_, t)| t.uid);
        self.focus_request = self.zoomed;
    }

    /// The tab of the focused pane (what pane-wide commands act on).
    pub(super) fn active_uid(&self) -> Option<u64> {
        let path = self.dock.focused_leaf()?;
        let leaf = self.dock.leaf(path).ok()?;
        leaf.tabs.get(leaf.active.0).map(|t| t.uid)
    }

    /// Gives the keyboard to a tab that just opened in the focused pane (new shell,
    /// split, connect), the way a new terminal window would have it.
    pub(super) fn focus_new_tab(&mut self) {
        let now: std::collections::HashSet<u64> = self.dock.iter_all_tabs().filter(|(p, _)| p.surface.is_main()).map(|(_, t)| t.uid).collect();
        if let Some(active) = self.active_uid()
            && !self.seen_tabs.contains(&active)
            && self.focus_request.is_none()
        {
            self.focus_request = Some(active);
        }
        self.seen_tabs = now;
    }

    /// Dimming / outline / focus inputs for this frame's tab views. Also hands a
    /// pending keyboard-focus request to its terminal (`TerminalView::want_focus`).
    pub(super) fn pane_fx(&mut self, ctx: &egui::Context) -> PaneFx {
        if let Some(uid) = self.focus_request.take()
            && let Some(body) = self.dock.iter_all_tabs_mut().map(|(_, t)| t).find(|t| t.uid == uid).map(|t| &mut t.body)
        {
            match body {
                TabBody::Terminal(view) => view.want_focus = true,
                TabBody::Sftp(view) => view.want_focus = true,
                TabBody::Editor(_) => {}
            }
        }
        let l = &settings::snapshot().layout;
        let overlay_open = self.palette.is_some() || self.settings_view.is_some() || ctx.memory(|m| m.top_modal_layer().is_some());
        PaneFx {
            active_uid: self.active_uid(),
            multi_pane: self.panes().len() > 1 && self.zoomed.is_none(),
            dim: if l.dim_inactive_panes { l.dim_amount() } else { 0.0 },
            outline: l.tab_bars_hidden(),
            focus_follows_mouse: l.focus_follows_mouse && !overlay_open,
            terminal_ids: std::mem::take(&mut self.terminal_ids),
        }
    }

    /// Applies what the views reported about focus (hover focus, terminal ids).
    pub(super) fn after_views(&mut self, viewer: &mut Viewer) {
        self.terminal_ids = std::mem::take(&mut viewer.terminal_ids);
        if let Some(uid) = viewer.hover_focus.take()
            && let Some(path) = self.dock.find_tab_from(|t| t.uid == uid)
        {
            self.dock.set_focused_node_and_surface(path.node_path());
            self.focus_request = Some(uid);
        }
    }

    /// Shows only the maximized tab. Returns false when nothing is zoomed (or it closed).
    pub(super) fn zoomed_view(&mut self, ui: &mut egui::Ui, viewer: &mut Viewer) -> bool {
        let Some(uid) = self.zoomed else { return false };
        let Some(tab) = self.dock.iter_all_tabs_mut().filter(|(p, _)| p.surface.is_main()).map(|(_, t)| t).find(|t| t.uid == uid) else {
            self.zoomed = None;
            return false;
        };
        let rect = ui.available_rect_before_wrap().shrink(6.0);
        let see_through = crate::themes::see_through();
        let fill = if see_through { theme::surface().gamma_multiply(crate::themes::window_opacity()) } else { theme::surface() };
        ui.painter().rect(
            rect,
            egui::CornerRadius::same(theme::RADIUS),
            fill,
            egui::Stroke::new(1.0, theme::border().gamma_multiply(0.55)),
            egui::StrokeKind::Inside,
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(("zoomed", uid)));
        child.set_clip_rect(rect);
        viewer.ui(&mut child, tab);
        // "Maximized" pill, top right: click (or the shortcut) restores the split.
        let text = format!("Maximized  ·  {} to restore", keybindings::label(Action::ZoomPane));
        let galley = ui.painter().layout_no_wrap(text, egui::FontId::proportional(11.5), theme::text());
        let pill = egui::Rect::from_min_size(egui::pos2(rect.right() - galley.size().x - 34.0, rect.top() + 8.0), galley.size() + egui::vec2(22.0, 10.0));
        let resp = ui.interact(pill, egui::Id::new("zoom_pill"), egui::Sense::click()).on_hover_text("Restore the split layout");
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Maximized pane: restore"));
        let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), theme::anim_time(0.1));
        let fg = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("zoom_pill_layer")));
        fg.rect(
            pill,
            egui::CornerRadius::same(11),
            theme::surface_2().lerp_to_gamma(theme::surface_3(), h),
            egui::Stroke::new(1.0, theme::accent().gamma_multiply(0.5)),
            egui::StrokeKind::Inside,
        );
        fg.galley(pill.min + egui::vec2(11.0, 5.0), galley, theme::text());
        if resp.clicked() {
            self.toggle_zoom();
        }
        true
    }

    pub(super) fn rename_modal(&mut self, ctx: &egui::Context) {
        let Some((uid, text)) = &mut self.rename else { return };
        let uid = *uid;
        let (mut done, mut cancel) = (false, false);
        let modal = egui::Modal::new(egui::Id::new("rename_tab")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading("Rename tab");
            ui.add_space(6.0);
            let edit = ui.add(egui::TextEdit::singleline(text).hint_text("Leave empty for the automatic title").desired_width(f32::INFINITY));
            if !edit.has_focus() && !edit.lost_focus() {
                edit.request_focus();
            }
            done |= edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                done |= ui.add(egui::Button::new("Rename").fill(theme::accent_soft())).clicked();
                cancel |= ui.button("Cancel").clicked();
            });
        });
        if cancel || modal.should_close() {
            self.rename = None;
        } else if done {
            let name = self.rename.take().map(|(_, t)| t);
            self.apply_tab_actions(vec![TabAction::Rename(uid, name)], ctx);
            self.focus_request = Some(uid);
        }
    }

    pub(super) fn welcome(&mut self, ui: &mut egui::Ui) {
        use crate::ui::welcome::{self, WelcomeAction};
        match welcome::show(ui, &self.store.profiles) {
            Some(WelcomeAction::Shell(s)) => self.open_local(Some(s), ui.ctx()),
            Some(WelcomeAction::Open(p)) => self.request_open(p, crate::ui::sidebar::OpenKind::Terminal, ui.ctx()),
            Some(WelcomeAction::NewHost) => self.open_editor_new(None),
            Some(WelcomeAction::Import) => self.import = Some(crate::ui::import_view::ImportDialog::new(&self.store)),
            Some(WelcomeAction::Palette) => self.open_palette(),
            None => {}
        }
    }

    pub(super) fn open_layout_settings(&mut self) {
        self.settings_view = Some(SettingsView::at(Section::Layout));
    }
}

/// Folds or opens a container.
pub(super) fn toggle_container(c: Container) {
    let _ = settings::update(|s| s.layout.views.toggle(c));
}

/// Brings `view` up (un-hiding it and opening its container).
pub(super) fn reveal_view(view: View) {
    let _ = settings::update(|s| s.layout.views.reveal(view));
}

pub(super) fn set_tab_position(p: TabPosition) {
    let _ = settings::update(|s| s.layout.tab_position = p);
}

pub(super) fn set_sidebar_side(side: Side) {
    let _ = settings::update(|s| s.layout.sidebar_side = side);
}

/// The dock with tab bars hidden on main-surface panes while vertical tabs are on
/// screen; floating `Surface::Window` panes always keep their tab bar and close button.
pub(super) fn sync_tab_bars(dock: &mut DockState<Tab>) -> bool {
    let hide = settings::snapshot().layout.tab_bars_hidden();
    for (path, leaf) in dock.iter_leaves_mut() {
        leaf.tab_bar_hidden = if path.surface.is_main() { hide } else { false };
    }
    hide
}
