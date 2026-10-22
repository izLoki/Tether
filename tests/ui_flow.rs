//! Headless UI tests: the real app driven through egui_kittest and rendered with wgpu.
//! Screenshots land in `target/ui-shots/`. The connected flow needs the e2e container
//! (see `e2e_ssh.rs`) and `TETHER_E2E=1`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tether::app::TetherApp;

const PROFILES: &str = r#"[
 {"id":"11111111-1111-4111-8111-111111111111","name":"Docker sshd","group":"Lab","host":"127.0.0.1","port":2222,"user":"tester","auth":"Password","hue":150,"pipeline_kb":4096,"remote_start_dir":"","last_used":1760000000},
 {"id":"22222222-2222-4222-8222-222222222222","name":"api-prod-01","group":"Production","host":"10.0.4.21","port":22,"user":"deploy","auth":"Password","hue":210,"pipeline_kb":4096,"remote_start_dir":"","last_used":0},
 {"id":"33333333-3333-4333-8333-333333333333","name":"db-replica","group":"Production","host":"10.0.4.40","port":22,"user":"postgres","auth":"Password","hue":30,"pipeline_kb":4096,"remote_start_dir":"","last_used":0}
]"#;

const WORKSPACES: &str = r#"{
 "Last session": {"Leaf": [{"profile_id": "11111111-1111-4111-8111-111111111111", "kind": "Terminal"}]},
 "Production": {"Split": {"side_by_side": true, "fraction": 0.5,
   "first": {"Leaf": [{"profile_id": "22222222-2222-4222-8222-222222222222", "kind": "Terminal"}]},
   "second": {"Leaf": [{"profile_id": "33333333-3333-4333-8333-333333333333", "kind": "Sftp"}, {"profile_id": "22222222-2222-4222-8222-222222222222", "kind": "Sftp"}]}}}
}"#;

fn harness(config: &std::path::Path) -> Harness<'static, TetherApp> {
    std::fs::write(config.join("profiles.json"), PROFILES).unwrap();
    std::fs::write(config.join("workspaces.json"), WORKSPACES).unwrap();
    // SAFETY: tests in this file run single-threaded (one #[test] touches the env at a time).
    unsafe { std::env::set_var("TETHER_CONFIG_DIR", config) };
    // Shell detection runs in the background; finish it first so the welcome screen
    // doesn't change layout between a query and its click.
    tether::pty::shells::available();
    Harness::builder().with_size(egui::vec2(1360.0, 820.0)).with_max_steps(10_000).wgpu().build_eframe(|cc| TetherApp::new(cc).expect("app init"))
}

fn shot(h: &mut Harness<'_, TetherApp>, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/ui-shots");
    std::fs::create_dir_all(&dir).unwrap();
    let img = h.render().expect("render");
    img.save(dir.join(format!("{name}.png"))).unwrap();
}

/// Steps in real time until `done` holds (network work happens on the app's runtime).
fn settle(h: &mut Harness<'_, TetherApp>, secs: u64, mut done: impl FnMut(&Harness<'_, TetherApp>) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        h.step();
        if done(h) {
            break;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    h.run_steps(20);
}

#[test]
fn ui_flows() {
    let config = tempfile::tempdir().unwrap();
    let mut h = harness(config.path());
    h.run_steps(30);
    // Sessions is the default view; the hosts live in the Hosts view.
    h.get_by_label("Hosts view").click();
    h.run_steps(10);
    h.get_by_label("Docker sshd");
    h.get_by_label("db-replica");
    shot(&mut h, "01-welcome");

    // Profile editor validates before saving.
    h.get_by_label("+  New host").click();
    h.run_steps(10);
    h.get_by_label("Save").click();
    h.run_steps(10);
    h.get_by_label("Host is required");
    shot(&mut h, "02-editor");
    h.get_by_label("Cancel").click();
    h.run_steps(10);

    // Import dialog (auto-detects Bitvise profiles / SSH config on this machine).
    h.get_by_label_contains("Import from Bitvise").click();
    h.run_steps(10);
    h.get_by_label("Import / export machines");
    shot(&mut h, "02b-import");
    h.get_by_label("Cancel").click();
    h.run_steps(10);

    // Key manager: generate an Ed25519 key without a passphrase.
    h.get_by_label("App menu").click();
    h.run_steps(10);
    h.get_by_label_contains("Key manager").click();
    h.run_steps(10);
    h.get_by_label("+ Generate").click();
    h.run_steps(10);
    shot(&mut h, "02c-key-generate");
    h.get_by_label("Generate key").click();
    settle(&mut h, 5, |h| h.query_by_label("Copy public key").is_some());
    h.get_by_label("Copy public key");
    shot(&mut h, "02d-key-details");
    h.key_press(Key::Escape);
    h.run_steps(10);

    // Hosts imported with a group show up inside folders (in the Hosts view).
    h.get_by_label("Production");
    h.get_by_label("Lab");

    sidebar_menus(&mut h);
    local_tabs_and_palette(&mut h);

    if std::env::var_os("TETHER_E2E").is_none() {
        return;
    }
    connected_flow(&mut h);
}

/// Two local shells in the Sessions view, then the command palette over them.
fn local_tabs_and_palette(h: &mut Harness<'_, TetherApp>) {
    let palette = |h: &mut Harness<'_, TetherApp>| {
        h.key_press_modifiers(Modifiers::CTRL | Modifiers::SHIFT, Key::P);
        h.run_steps(10);
    };
    palette(h);
    h.event(egui::Event::Text("vertical tabs left".into()));
    h.run_steps(5);
    h.key_press(Key::Enter);
    h.run_steps(10);
    for _ in 0..2 {
        h.key_press_modifiers(Modifiers::CTRL | Modifiers::SHIFT, Key::T);
        h.run_steps(5);
    }
    settle(h, 4, |_| false);
    shot(h, "12-vertical-tabs");
    palette(h);
    shot(h, "13-palette");
    h.key_press(Key::Escape);
    h.run_steps(10);
}

/// Right-click menus on folders, hosts and workspaces, the "New" dropdown and the
/// inline "save workspace" field.
fn sidebar_menus(h: &mut Harness<'_, TetherApp>) {
    h.get_by_label("Workspaces view").click();
    h.run_steps(10);
    h.get_by_label("Workspace Production").hover();
    h.run_steps(10);
    shot(h, "06-sidebar-workspaces");
    workspace_menus(h);

    h.get_by_label("Hosts view").click();
    h.run_steps(10);
    h.get_by_label("Production").click_secondary();
    h.run_steps(10);
    h.get_by_label_contains("New subfolder");
    shot(h, "07-folder-menu");
    h.key_press(Key::Escape);
    h.run_steps(10);

    h.get_by_label("db-replica").click_secondary();
    h.run_steps(10);
    h.get_by_label_contains("Connect terminal");
    shot(h, "08-host-menu");
    h.key_press(Key::Escape);
    h.run_steps(10);

    // Right-click on the empty space below the hosts.
    let empty = egui::pos2(130.0, 440.0);
    h.hover_at(empty);
    h.run_steps(2);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: empty, button: egui::PointerButton::Secondary, pressed, modifiers: Modifiers::NONE });
        h.run_steps(1);
    }
    h.run_steps(10);
    h.get_by_label_contains("Search hosts");
    h.get_by_label_contains("Key manager");
    shot(h, "08b-background-menu");
    h.key_press(Key::Escape);
    h.run_steps(10);

    h.get_by_label("New menu").click();
    h.run_steps(10);
    h.get_by_label_contains("Import / export setup");
    shot(h, "09-new-menu");
    h.key_press(Key::Escape);
    h.run_steps(10);
}

/// The workspace row's menu and its delete confirmation (Workspaces view showing).
fn workspace_menus(h: &mut Harness<'_, TetherApp>) {
    h.get_by_label("Workspace Production").click_secondary();
    h.run_steps(10);
    h.get_by_label_contains("Overwrite with current layout");
    shot(h, "10-workspace-menu");
    h.get_by_label_contains("Delete…").click();
    h.run_steps(10);
    h.get_by_label("Delete workspace?");
    shot(h, "11-workspace-delete");
    h.get_by_label("Cancel").click();
    h.run_steps(10);
    h.get_by_label("Workspace Production");
}

const TEST_PROFILE: &str = "11111111-1111-4111-8111-111111111111";

/// The prompt's "Remember" box writes to the OS vault; keep the test from leaving it behind.
struct ForgetSecret;
impl Drop for ForgetSecret {
    fn drop(&mut self) {
        let _ = tether::profile::secret::delete(TEST_PROFILE.parse().unwrap());
    }
}

fn connected_flow(h: &mut Harness<'_, TetherApp>) {
    let _ = tether::profile::secret::delete(TEST_PROFILE.parse().unwrap());
    let _cleanup = ForgetSecret;
    h.get_by_label_contains("tester@127.0.0.1:2222").click();
    h.run_steps(10);
    let pw = h.get_by_role(egui::accesskit::Role::PasswordInput);
    pw.focus();
    pw.type_text("tetherpass");
    h.run_steps(2);
    h.get_by_label("Connect").click();
    settle(h, 10, |h| h.query_by_label("Connection failed").is_some() || h.query_by_label("Connecting…").is_none());
    h.run_steps(10);

    h.get_by_label("Terminal").click();
    h.run_steps(5);
    h.get_by_label("Terminal").type_text("printf '\\033[1;32mhello from tether\\033[0m\\n'; uname -a");
    h.key_press(Key::Enter);
    settle(h, 3, |_| false);
    shot(h, "03-terminal");

    // Ctrl+Shift+F opens SFTP next to the focused terminal.
    h.key_press_modifiers(Modifiers::CTRL | Modifiers::SHIFT, Key::F);
    settle(h, 8, |h| h.query_by_label_contains("REMOTE").is_some() && h.query_by_label("Connecting…").is_none());
    shot(h, "04-split-sftp");

    // Ctrl+Shift+E splits another terminal below.
    h.get_by_label("Terminal").click();
    h.key_press_modifiers(Modifiers::CTRL | Modifiers::SHIFT, Key::E);
    settle(h, 5, |_| false);
    shot(h, "05-three-panes");
}
