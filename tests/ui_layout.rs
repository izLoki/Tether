//! Headless screenshots of the layout features (activity strips and movable views,
//! side bars, the bottom panel, drag-and-drop drop zones, command palette, dimmed
//! panes, maximized pane, welcome screen), with real local shells. PNGs land in
//! `target/ui-shots/`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, Key, Modifiers, PointerButton};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tether::app::TetherApp;
use tether::layout::{BarOrientation, Container, Side, TabPosition, View};

const PROFILES: &str = r#"[
 {"id":"22222222-2222-4222-8222-222222222222","name":"api-prod-01","group":"Production","host":"10.0.4.21","port":22,"user":"deploy","auth":"Password","hue":210,"pipeline_kb":4096,"remote_start_dir":"","last_used":1760000000},
 {"id":"33333333-3333-4333-8333-333333333333","name":"db-replica","group":"Production","host":"10.0.4.40","port":22,"user":"postgres","auth":"Password","hue":30,"pipeline_kb":4096,"remote_start_dir":"","last_used":1750000000},
 {"id":"44444444-4444-4444-8444-444444444444","name":"build-box","group":"Lab","host":"192.168.1.20","port":22,"user":"ci","auth":"Password","hue":280,"pipeline_kb":4096,"remote_start_dir":"","last_used":0}
]"#;

fn harness(config: &std::path::Path) -> Harness<'static, TetherApp> {
    std::fs::write(config.join("profiles.json"), PROFILES).unwrap();
    // SAFETY: this file has a single #[test]; nothing else reads the env concurrently.
    unsafe { std::env::set_var("TETHER_CONFIG_DIR", config) };
    Harness::builder().with_size(egui::vec2(1360.0, 820.0)).with_max_steps(10_000).wgpu().build_eframe(|cc| TetherApp::new(cc).expect("app init"))
}

fn shot(h: &mut Harness<'_, TetherApp>, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/ui-shots");
    std::fs::create_dir_all(&dir).unwrap();
    h.render().expect("render").save(dir.join(format!("{name}.png"))).unwrap();
}

/// Steps in real time (shells start on their own threads).
fn settle(h: &mut Harness<'_, TetherApp>, secs: f32) {
    let deadline = Instant::now() + Duration::from_secs_f32(secs);
    while Instant::now() < deadline {
        h.step();
        std::thread::sleep(Duration::from_millis(30));
    }
    h.run_steps(10);
}

fn keys(h: &mut Harness<'_, TetherApp>, m: Modifiers, k: Key) {
    let action = if m == CS {
        match k {
            Key::T => Some(tether::keybindings::Action::NewLocalTerminal),
            Key::D => Some(tether::keybindings::Action::SplitRight),
            Key::E => Some(tether::keybindings::Action::SplitDown),
            _ => None,
        }
    } else {
        None
    };
    if let Some(b) = action.and_then(|a| a.default_binding()) {
        h.key_press_modifiers(b.modifiers(), b.egui_key().unwrap());
    } else {
        h.key_press_modifiers(m, k);
    }
    h.run_steps(4);
}

fn layout(f: impl FnOnce(&mut tether::layout::LayoutSettings)) {
    tether::settings::update(|s| f(&mut s.layout)).unwrap();
}

fn views() -> tether::layout::ViewLayout {
    tether::settings::snapshot().layout.views.clone()
}

fn pointer(h: &mut Harness<'_, TetherApp>, pos: egui::Pos2, pressed: Option<bool>) {
    h.event(egui::Event::PointerMoved(pos));
    if let Some(pressed) = pressed {
        h.event(egui::Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
    }
    h.run_steps(2);
}

const CS: Modifiers = Modifiers { ctrl: !cfg!(target_os = "macos"), shift: true, alt: false, mac_cmd: cfg!(target_os = "macos"), command: true };

#[test]
fn layout_screens() {
    let config = tempfile::tempdir().unwrap();
    let mut h = harness(config.path());
    h.run_steps(30);
    shot(&mut h, "20-welcome");
    if tether::pty::shells::default_shell().is_none() {
        eprintln!("no local shell: skipping the terminal layouts");
        return;
    }

    // Three local shells: two tabs in one pane, one split to the right.
    keys(&mut h, CS, Key::T);
    keys(&mut h, CS, Key::T);
    settle(&mut h, 1.0);
    h.get_all_by_label("Terminal").last().unwrap().click();
    h.run_steps(4);
    keys(&mut h, CS, Key::D);
    settle(&mut h, 2.5);

    // Default: the Sessions view active in the primary side bar, no tab strips.
    assert!(views().is_shown(View::Sessions));
    shot(&mut h, "21-views-default");
    h.get_by_label("Hosts view").hover();
    h.run_steps(20);
    shot(&mut h, "21b-activity-hover");

    // Clicking the active icon folds the side bar to its strip; again unfolds it.
    h.get_by_label("Sessions view").click();
    settle(&mut h, 0.5);
    assert!(views().primary.collapsed);
    shot(&mut h, "22-primary-folded");
    h.get_by_label("Sessions view").click();
    settle(&mut h, 0.5);
    assert!(!views().primary.collapsed);

    // Right-click an icon: Move to / Hide / views / icon placement.
    h.get_by_label("Agents view").click_secondary();
    h.run_steps(10);
    h.get_by_label_contains("Move to");
    shot(&mut h, "23-icon-menu");
    h.key_press(Key::Escape);
    h.run_steps(10);

    // Views split between both side bars.
    layout(|l| {
        l.views.move_view(View::Hosts, Container::Secondary, None);
        l.views.move_view(View::Agents, Container::Secondary, None);
        l.views.secondary.active = Some(View::Hosts);
    });
    settle(&mut h, 0.6);
    shot(&mut h, "24-views-split");

    // Icons on top of the side bars.
    layout(|l| {
        l.views.primary.bar = BarOrientation::Top;
        l.views.secondary.bar = BarOrientation::Top;
    });
    settle(&mut h, 0.6);
    shot(&mut h, "25-activity-top");

    // Sessions in the bottom panel (compact columns, one per pane).
    layout(|l| {
        l.views.primary.bar = BarOrientation::Side;
        l.views.secondary.bar = BarOrientation::Side;
        l.views.move_view(View::Sessions, Container::Panel, None);
        l.views.move_view(View::Agents, Container::Panel, None);
        l.views.panel.active = Some(View::Sessions);
    });
    settle(&mut h, 0.6);
    shot(&mut h, "26-view-in-panel");

    // Drag the Workspaces icon: drop zones on the window edges, then drop it on the
    // bottom edge (the panel).
    tether::ui::activity::forget_sizes(&h.ctx);
    layout(|l| l.reset_views());
    settle(&mut h, 0.6);
    let icon = h.get_by_label("Workspaces view").rect().center();
    pointer(&mut h, icon, None);
    pointer(&mut h, icon, Some(true));
    for k in 1..=12 {
        let t = k as f32 / 12.0;
        pointer(&mut h, icon + egui::vec2(600.0 * t, 560.0 * t), None);
    }
    h.run_steps(10);
    shot(&mut h, "27-drag-drop-zones");
    let drop_at = icon + egui::vec2(600.0, 560.0);
    pointer(&mut h, drop_at, Some(false));
    settle(&mut h, 0.6);
    assert_eq!(views().container_of(View::Workspaces), Some(Container::Panel), "dropped on the bottom edge");
    shot(&mut h, "27b-dropped-in-panel");

    // Reorder within the primary strip: Agents before Sessions.
    let from = h.get_by_label("Agents view").rect().center();
    let to = h.get_by_label("Sessions view").rect().center() - egui::vec2(0.0, 14.0);
    pointer(&mut h, from, None);
    pointer(&mut h, from, Some(true));
    for k in 1..=8 {
        pointer(&mut h, from + (to - from) * (k as f32 / 8.0), None);
    }
    shot(&mut h, "28-drag-reorder");
    pointer(&mut h, to, Some(false));
    h.run_steps(10);
    assert_eq!(views().primary.views.first(), Some(&View::Agents), "reordered: {:?}", views().primary.views);

    // Tabs on top of the panes, side bar on the right, panes dimmed.
    tether::ui::activity::forget_sizes(&h.ctx);
    layout(|l| {
        l.reset_views();
        l.tab_position = TabPosition::Top;
        l.sidebar_side = Side::Right;
        l.dim_inactive_panes = true;
        l.views.click(View::Hosts);
    });
    settle(&mut h, 0.6);
    shot(&mut h, "29-tabs-top-sidebar-right");

    // Command palette with a query.
    keys(&mut h, CS, Key::P);
    h.run_steps(10);
    shot(&mut h, "23a-palette-open");
    h.event(egui::Event::Text("move view".into()));
    h.run_steps(10);
    shot(&mut h, "23b-palette-views");
    h.key_press(Key::Escape);
    h.run_steps(10);
    assert!(h.query_by_label_contains("Palette ").is_none(), "Esc closes the palette");

    // Maximize the focused pane.
    layout(|l| {
        l.tab_position = TabPosition::Sidebar;
        l.sidebar_side = Side::Left;
        l.views.click(View::Sessions);
    });
    keys(&mut h, CS, Key::Enter);
    settle(&mut h, 0.6);
    h.get_by_label_contains("Maximized");
    shot(&mut h, "30-zoomed-pane");
    keys(&mut h, CS, Key::Enter);
    h.run_steps(10);

    // Settings → Layout with its schematic.
    layout(|l| l.views.move_view(View::Agents, Container::Secondary, None));
    keys(&mut h, CS, Key::P);
    h.event(egui::Event::Text("layout settings".into()));
    h.run_steps(5);
    h.key_press(Key::Enter);
    settle(&mut h, 0.8);
    h.get_by_label_contains("Drag an icon onto another strip");
    shot(&mut h, "31-layout-settings");
    // MacBook-sized windows and Retina scaling must keep the settings usable.
    h.set_size(egui::vec2(760.0, 480.0));
    h.run_steps(20);
    h.get_by_label_contains("Drag an icon onto another strip");
    shot(&mut h, "32-layout-minimum");
    // Native display density is independent of the user's UI zoom preference.
    h.input_mut().viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(2.0);
    h.run_steps(20);
    assert_eq!(h.ctx.pixels_per_point(), 2.0);
    shot(&mut h, "33-layout-retina");
}
