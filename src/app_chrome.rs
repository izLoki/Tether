//! The top bar: the app menu and sidebar toggle on the left, the command palette pill in
//! the middle, a few icon buttons on the right (agents needing attention, broadcast,
//! settings). Everything clickable is a button with a hover background, a pointer and a
//! tooltip naming its shortcut; information is dim text without hover.
//! A child module of `app`.

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Response, RichText, Sense, Stroke, Vec2};

use super::TetherApp;
use crate::keybindings::{self, Action};
use crate::layout::{Container, Side, TabPosition};
use crate::settings;
use crate::theme;
use crate::ui::import_view::ImportDialog;
use crate::ui::settings_widgets::{self as w, Icon, paint_icon};

const BAR_H: f32 = 40.0;
/// Height of every button in the bar.
const BTN_H: f32 = 28.0;
/// Space between the bar's buttons.
const GAP: f32 = 8.0;
const ICON: f32 = 15.0;
const PILL_W: f32 = 380.0;
const MENU_W: f32 = 250.0;
/// Below this bar width the palette pill collapses to a search button.
const NARROW: f32 = 760.0;

/// How a bar button is tinted.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum ButtonTone {
    Normal,
    /// Needs a look (agents waiting): warm fill.
    Warn,
    /// A toggle that is on, in this colour.
    On(Color32),
}

/// A bar button: an icon and/or a label, 28 px high, hover background, pointer cursor
/// and `tip` (with its shortcut) on hover.
pub(super) fn chrome_button(ui: &mut egui::Ui, icon: Option<Icon>, text: Option<RichText>, tone: ButtonTone, tip: &str) -> Response {
    let galley = text.map(|t| egui::WidgetText::from(t).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button));
    let pad = 7.0;
    let icon_w = if icon.is_some() { ICON } else { 0.0 };
    let between = if icon.is_some() && galley.is_some() { 6.0 } else { 0.0 };
    let text_w = galley.as_ref().map_or(0.0, |g| g.size().x);
    let width = (pad * 2.0 + icon_w + between + text_w).max(BTN_H);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, BTN_H), Sense::click());
    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() || resp.is_pointer_button_down_on(), theme::anim_time(0.1));
    let (fill, stroke, ink) = match tone {
        ButtonTone::Normal => (theme::surface_3().gamma_multiply(h), Color32::TRANSPARENT, theme::text_dim().lerp_to_gamma(theme::text(), h)),
        ButtonTone::Warn => (theme::warn().gamma_multiply(0.14 + 0.1 * h), theme::warn().gamma_multiply(0.45), theme::warn()),
        ButtonTone::On(c) => (c.gamma_multiply(0.18 + 0.08 * h), c.gamma_multiply(0.5), c),
    };
    let p = ui.painter();
    p.rect(rect, CornerRadius::same(7), fill, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    let content_w = icon_w + between + text_w;
    let mut x = rect.center().x - content_w / 2.0;
    if let Some(icon) = icon {
        paint_icon(p, Rect::from_center_size(Pos2::new(x + ICON / 2.0, rect.center().y), Vec2::splat(ICON)), icon, ink);
        x += ICON + between;
    }
    if let Some(g) = galley {
        p.galley(Pos2::new(x, rect.center().y - g.size().y / 2.0), g, ink);
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text(tip)
}

/// An icon button whose accessible name is its tooltip's first line.
fn icon_btn(ui: &mut egui::Ui, icon: Icon, tone: ButtonTone, name: &str, tip: &str) -> Response {
    let resp = chrome_button(ui, Some(icon), None, tone, tip);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    resp
}

impl TetherApp {
    pub(super) fn top_bar(&mut self, ui: &mut egui::Ui) {
        let frame = egui::Frame::new().fill(theme::bg()).inner_margin(egui::Margin::symmetric(8, 6));
        egui::Panel::top("top").exact_size(BAR_H).frame(frame).show(ui, |ui| {
            let bar = ui.max_rect();
            let narrow = bar.width() < NARROW;
            if !narrow {
                self.palette_pill(ui, bar);
            }
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                // The sidebar toggle sits on the sidebar's side of the bar.
                let sidebar_right = settings::snapshot().layout.sidebar_side == Side::Right;
                self.app_menu(ui);
                if !sidebar_right {
                    sidebar_toggle(ui);
                }
                if narrow
                    && icon_btn(ui, Icon::Search, ButtonTone::Normal, "Command palette", &keybindings::hint("Search or run a command", Action::CommandPalette))
                        .clicked()
                {
                    self.open_palette();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    if sidebar_right {
                        sidebar_toggle(ui);
                    }
                    if icon_btn(ui, Icon::Gear, ButtonTone::Normal, "Settings", &keybindings::hint("Settings", Action::OpenSettings)).clicked() {
                        self.settings_view.get_or_insert_with(crate::ui::settings_view::SettingsView::new);
                    }
                    self.broadcast_button(ui);
                    self.agents_top_bar(ui);
                    crate::ui::update_banner::top_bar(ui);
                });
            });
        });
    }

    /// The menu behind the hamburger: what used to crowd the bar (import, keys, layout),
    /// and the app's resource use as dim text.
    fn app_menu(&mut self, ui: &mut egui::Ui) {
        let usage = crate::ui::usage_chip::total();
        let mut tip = "Menu: import, keys, layout, settings".to_string();
        if let Some((text, _)) = &usage {
            tip.push_str(&format!("\n{text} (tether and its processes)"));
        }
        let resp = icon_btn(ui, Icon::Menu, ButtonTone::Normal, "App menu", &tip);
        egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.set_width(MENU_W);
            let ctx = ui.ctx().clone();
            let item = |ui: &mut egui::Ui, label: &str, action: Option<Action>| {
                let keys = action.map(keybindings::label).unwrap_or_default();
                ui.add(egui::Button::new(RichText::new(label).size(12.5)).shortcut_text(keys).min_size(Vec2::new(MENU_W, 24.0))).clicked()
            };
            if item(ui, "New terminal", Some(Action::NewLocalTerminal)) {
                self.run_action(Action::NewLocalTerminal, &ctx);
            }
            if item(ui, "Command palette…", Some(Action::CommandPalette)) {
                self.open_palette();
            }
            ui.separator();
            if item(ui, "Import / export setup…", None) {
                self.import = Some(ImportDialog::new(&self.store));
                ui.close();
            }
            if item(ui, "Key manager…", Some(Action::KeyManager)) {
                self.run_action(Action::KeyManager, &ctx);
            }
            ui.separator();
            ui.menu_button(RichText::new("Layout").size(12.5), |ui| self.layout_menu(ui));
            if item(ui, "Agent sessions", Some(Action::ToggleAgents)) {
                self.toggle_agents();
            }
            if item(ui, "Codex sessions", Some(Action::ToggleCodex)) {
                self.run_action(Action::ToggleCodex, &ctx);
            }
            if item(ui, "Settings…", Some(Action::OpenSettings)) {
                self.run_action(Action::OpenSettings, &ctx);
            }
            ui.separator();
            let mut footer = format!("tether {}", env!("CARGO_PKG_VERSION"));
            if let Some((text, _)) = &usage {
                footer.push_str(&format!("  ·  {text}"));
            }
            let info = ui.label(RichText::new(footer).size(10.5).color(theme::text_faint()));
            if let Some((_, total)) = &usage {
                info.on_hover_ui(|ui| crate::ui::usage_chip::breakdown(ui, total));
            }
        });
    }

    /// "Search or run a command…" centred in the bar; opens the palette.
    fn palette_pill(&mut self, ui: &mut egui::Ui, bar: Rect) {
        let width = PILL_W.min(bar.width() * 0.36);
        let rect = Rect::from_center_size(bar.center(), Vec2::new(width, BTN_H));
        let resp = ui.interact(rect, egui::Id::new("palette_pill"), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Command palette"));
        let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), theme::anim_time(0.12));
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let p = ui.painter();
        let fill = theme::surface().lerp_to_gamma(theme::surface_2(), 0.4 + 0.6 * h);
        p.rect(rect, CornerRadius::same(8), fill, Stroke::new(1.0, theme::border().lerp_to_gamma(theme::text_faint(), 0.35 * h)), egui::StrokeKind::Inside);
        let ink = theme::text_faint().lerp_to_gamma(theme::text_dim(), 0.5 + 0.5 * h);
        paint_icon(p, Rect::from_center_size(Pos2::new(rect.left() + 17.0, rect.center().y), Vec2::splat(13.0)), Icon::Search, ink);
        p.text(Pos2::new(rect.left() + 32.0, rect.center().y), Align2::LEFT_CENTER, "Search or run a command…", FontId::proportional(12.5), ink);
        let keys = keybindings::label(Action::CommandPalette);
        if !keys.is_empty() {
            // A key cap, so the shortcut reads as a hint rather than as more text.
            let galley = p.layout_no_wrap(keys, FontId::monospace(10.5), theme::text_dim());
            let cap =
                Rect::from_min_size(Pos2::new(rect.right() - 6.0 - galley.size().x - 12.0, rect.center().y - 9.0), Vec2::new(galley.size().x + 12.0, 18.0));
            p.rect(cap, CornerRadius::same(5), theme::surface_3().gamma_multiply(0.8), Stroke::new(1.0, theme::border()), egui::StrokeKind::Inside);
            p.galley(Pos2::new(cap.left() + 6.0, cap.center().y - galley.size().y / 2.0), galley, theme::text_dim());
        }
        let resp = resp.on_hover_text(keybindings::hint("Search tabs, hosts and commands", Action::CommandPalette));
        if resp.clicked() {
            self.open_palette();
        }
    }

    fn broadcast_button(&mut self, ui: &mut egui::Ui) {
        let tone = if self.broadcast { ButtonTone::On(theme::danger()) } else { ButtonTone::Normal };
        let state = if self.broadcast { "Broadcast is on: typing goes to every terminal" } else { "Broadcast: type into every terminal at once" };
        if icon_btn(ui, Icon::Broadcast, tone, "Broadcast", &keybindings::hint(state, Action::ToggleBroadcast)).clicked() {
            self.broadcast = !self.broadcast;
        }
    }

    /// The layout submenu: the most used arrangement switches in one place.
    fn layout_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_width(280.0);
        let before = settings::snapshot().layout.clone();
        let mut l = before.clone();
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(RichText::new("TABS").size(10.5).color(theme::text_faint()));
        w::segmented(ui, &mut l.tab_position, &[(TabPosition::Sidebar, "Sessions view"), (TabPosition::Top, "Top of panes")]);
        ui.add_space(4.0);
        ui.label(RichText::new("SIDE BARS & PANEL").size(10.5).color(theme::text_faint()));
        ui.horizontal(|ui| {
            ui.label(RichText::new("Primary on the").size(12.0).color(theme::text_dim()));
            w::segmented(ui, &mut l.sidebar_side, &[(Side::Left, "Left"), (Side::Right, "Right")]);
        });
        for (c, action) in
            [(Container::Primary, Action::ToggleSidebar), (Container::Secondary, Action::ToggleSecondarySidebar), (Container::Panel, Action::TogglePanel)]
        {
            let state = l.views.get_mut(c);
            let mut open = !state.collapsed;
            let label = if state.is_empty() { format!("{} (empty)", c.title()) } else { c.title().to_string() };
            let resp = ui.add_enabled(!state.is_empty(), egui::Checkbox::new(&mut open, label));
            if resp.changed() {
                state.collapsed = !open;
            }
            resp.on_hover_text(keybindings::hint("Show or fold", action));
        }
        if ui.add(egui::Button::new("Reset view layout").shortcut_text(keybindings::label(Action::ResetLayout))).clicked() {
            super::views_ui::reset_layout(ui.ctx());
            ui.close();
            return;
        }
        ui.add_space(4.0);
        ui.label(RichText::new("PANES").size(10.5).color(theme::text_faint()));
        ui.checkbox(&mut l.dim_inactive_panes, "Dim inactive panes");
        ui.checkbox(&mut l.focus_follows_mouse, "Focus follows mouse");
        let zoom = if self.zoomed.is_some() { "Restore panes" } else { "Maximize focused pane" };
        if ui.add(egui::Button::new(zoom).shortcut_text(keybindings::label(Action::ZoomPane))).clicked() {
            self.toggle_zoom();
            ui.close();
        }
        ui.separator();
        if ui.button("All layout settings…").clicked() {
            self.open_layout_settings();
            ui.close();
        }
        if l != before {
            let _ = settings::update(|s| s.layout = l);
        }
    }
}

fn sidebar_toggle(ui: &mut egui::Ui) {
    let l = settings::snapshot().layout.clone();
    let icon = if l.sidebar_side == Side::Right { Icon::SidebarRight } else { Icon::SidebarLeft };
    let tip = keybindings::hint(if l.views.primary.collapsed { "Show primary side bar" } else { "Hide primary side bar" }, Action::ToggleSidebar);
    if icon_btn(ui, icon, ButtonTone::Normal, "Toggle sidebar", &tip).clicked() {
        super::layout_ui::toggle_container(Container::Primary);
    }
}
