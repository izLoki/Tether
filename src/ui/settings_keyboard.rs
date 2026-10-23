//! Settings → Keyboard: every rebindable shortcut, grouped. Click a binding to record
//! a new one (Esc cancels, Backspace unbinds); a binding taken from another action is
//! moved, and the row says so.

use eframe::egui::{self, Align, CornerRadius, Event, FontId, Key, Layout, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};

use super::settings_widgets as w;
use crate::keybindings::{self, Action, Binding, Group};
use crate::settings::Settings;
use crate::theme;

#[derive(Default)]
pub struct KeyboardState {
    recording: Option<Action>,
    /// (action, message) shown under a row after a change, e.g. "Moved from Next tab".
    note: Option<(Action, String)>,
}

impl KeyboardState {
    /// Stops a recording in progress (e.g. the user switched section).
    pub fn cancel(&mut self) {
        stop(self);
    }
}

/// The binding `action` has in the draft settings.
fn effective(s: &Settings, action: Action) -> Option<Binding> {
    s.keybindings.get(&action).cloned().unwrap_or_else(|| action.default_binding())
}

fn set(s: &mut Settings, action: Action, binding: Option<Binding>) {
    if binding == action.default_binding() {
        s.keybindings.remove(&action);
    } else {
        s.keybindings.insert(action, binding);
    }
}

pub fn show(ui: &mut Ui, s: &mut Settings, st: &mut KeyboardState, card_gap: f32) {
    if st.recording.is_some() {
        record(ui, s, st);
    }
    for (gi, group) in Group::ALL.into_iter().enumerate() {
        if gi > 0 {
            ui.add_space(card_gap);
        }
        w::card(ui, Some(group.title()), |ui| {
            let actions: Vec<Action> = Action::ALL.into_iter().filter(|a| a.group() == group).collect();
            for (i, action) in actions.into_iter().enumerate() {
                if i > 0 {
                    w::separator(ui);
                }
                row(ui, s, st, action);
            }
        });
    }
    ui.add_space(card_gap);
    w::card(ui, None, |ui| {
        w::row(ui, "Reset all shortcuts", "Back to the defaults listed in the README", |ui| {
            if w::ghost_button(ui, "Reset all", theme::text()).clicked() {
                s.keybindings.clear();
                st.note = None;
                stop(st);
            }
        });
    });
}

fn row(ui: &mut Ui, s: &mut Settings, st: &mut KeyboardState, action: Action) {
    ui.horizontal(|ui| {
        ui.set_min_height(30.0);
        ui.label(RichText::new(action.title()).size(13.0));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let current = effective(s, action);
            let customised = current != action.default_binding();
            if customised && reset_button(ui).on_hover_text("Restore the default").clicked() {
                st.note = match action.default_binding() {
                    Some(default) => assign(s, action, default),
                    None => {
                        set(s, action, None);
                        None
                    }
                };
            }
            let recording = st.recording == Some(action);
            if binding_button(ui, current.as_ref(), recording).clicked() {
                if recording {
                    stop(st);
                } else {
                    st.recording = Some(action);
                    st.note = None;
                    keybindings::set_recording(true);
                }
            }
        });
    });
    if let Some((a, note)) = &st.note
        && *a == action
    {
        ui.label(RichText::new(note).size(11.5).color(theme::warn()));
    }
}

/// Reads this frame's key presses into the action being recorded.
fn record(ui: &mut Ui, s: &mut Settings, st: &mut KeyboardState) {
    let Some(action) = st.recording else { return };
    let pressed = ui.input_mut(|i| {
        let found = i.events.iter().find_map(|e| match e {
            Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
            _ => None,
        });
        if found.is_some() {
            // Don't let Tab move focus or Enter click something while recording.
            i.events.retain(|e| !matches!(e, Event::Key { .. } | Event::Text(_)));
        }
        found
    });
    let Some((key, m)) = pressed else { return };
    match key {
        Key::Escape => {}
        Key::Backspace | Key::Delete if !m.any() => {
            set(s, action, None);
            st.note = Some((action, "Unbound".into()));
        }
        _ if !(m.ctrl || m.command || m.alt || is_function_key(key)) => {
            // A bare key would be stolen from the terminal and every text field.
            st.note = Some((action, "Use Command, Control or Alt/Option with the key (function keys work alone)".into()));
        }
        _ => st.note = assign(s, action, Binding::from_event(key, m)),
    }
    stop(st);
}

/// Gives `binding` to `action`, unbinding any other action that had it.
fn assign(s: &mut Settings, action: Action, binding: Binding) -> Option<(Action, String)> {
    let taken: Vec<Action> = Action::ALL.into_iter().filter(|a| *a != action && effective(s, *a).as_ref() == Some(&binding)).collect();
    for other in &taken {
        set(s, *other, None);
    }
    set(s, action, Some(binding));
    (!taken.is_empty()).then(|| {
        let names: Vec<&str> = taken.iter().map(|a| a.title()).collect();
        (action, format!("Moved from “{}” (now unbound)", names.join("”, “")))
    })
}

fn is_function_key(key: Key) -> bool {
    key.name().strip_prefix('F').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn stop(st: &mut KeyboardState) {
    st.recording = None;
    keybindings::set_recording(false);
}

/// Key caps for the binding, or a pulsing "Press keys…" while recording.
fn binding_button(ui: &mut Ui, binding: Option<&Binding>, recording: bool) -> egui::Response {
    let font = FontId::monospace(11.5);
    let caps: Vec<String> = match (recording, binding) {
        (true, _) => vec!["Press keys…  Esc cancels · ⌫ unbinds".into()],
        (false, Some(b)) => b.caps(),
        (false, None) => vec!["Not set".into()],
    };
    let galleys: Vec<_> = caps.iter().map(|c| ui.painter().layout_no_wrap(c.clone(), font.clone(), theme::text())).collect();
    let gap = 6.0;
    let width = galleys.iter().map(|g| g.size().x + 12.0).sum::<f32>() + gap * (galleys.len().saturating_sub(1)) as f32 + 12.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width.max(80.0), 28.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), theme::anim_time(0.12));
    let p = ui.painter();
    let border = if recording {
        let pulse = 0.5 + 0.5 * (ui.input(|i| i.time) * 5.0).sin() as f32;
        ui.ctx().request_repaint();
        theme::accent().gamma_multiply(0.5 + 0.5 * pulse)
    } else {
        theme::border().lerp_to_gamma(theme::accent(), h * 0.6)
    };
    p.rect(rect, CornerRadius::same(8), theme::surface().gamma_multiply(0.6 + 0.4 * h), Stroke::new(1.0, border), egui::StrokeKind::Inside);
    let mut x = rect.left() + 6.0;
    let dim = binding.is_none() && !recording;
    for g in galleys {
        let cap = Rect::from_min_size(Pos2::new(x, rect.center().y - g.size().y / 2.0 - 3.0), g.size() + Vec2::new(12.0, 6.0));
        if !recording && !dim {
            p.rect_filled(cap.translate(Vec2::new(0.0, 1.5)), CornerRadius::same(5), theme::border());
            p.rect(cap, CornerRadius::same(5), theme::surface_3(), Stroke::new(1.0, theme::border()), egui::StrokeKind::Inside);
        }
        let col = if dim {
            theme::text_faint()
        } else if recording {
            theme::accent()
        } else {
            theme::text()
        };
        p.galley(cap.min + Vec2::new(6.0, 3.0), g, col);
        x = cap.right() + gap;
    }
    resp
}

fn reset_button(ui: &mut Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), theme::anim_time(0.1));
    let p = ui.painter();
    if h > 0.0 {
        p.circle_filled(rect.center(), 11.0, theme::surface_3().gamma_multiply(h));
    }
    let col = theme::text_dim().lerp_to_gamma(theme::text(), h);
    let c = rect.center();
    let pts: Vec<Pos2> = (0..=14)
        .map(|i| {
            let a = -0.6 + 4.6 * i as f32 / 14.0;
            c + Vec2::new(a.cos(), a.sin()) * 6.0
        })
        .collect();
    let head = pts[0];
    p.add(egui::Shape::line(pts, Stroke::new(1.5, col)));
    p.line_segment([head, head + Vec2::new(-3.5, -1.0)], Stroke::new(1.5, col));
    p.line_segment([head, head + Vec2::new(0.5, -3.5)], Stroke::new(1.5, col));
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_the_default_clears_the_override() {
        let mut s = Settings::default();
        set(&mut s, Action::CloseTab, None);
        assert_eq!(effective(&s, Action::CloseTab), None);
        set(&mut s, Action::CloseTab, Action::CloseTab.default_binding());
        assert!(s.keybindings.is_empty());
    }
}
