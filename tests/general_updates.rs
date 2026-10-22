//! Exercises both sidebar custom launchers and the portable setup disclosure.
use eframe::egui::{self, Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tether::app::TetherApp;

#[test]
fn custom_boot_and_export_flow() {
    let config = tempfile::tempdir().unwrap();
    let project = config.path().join("PrismaCore project");
    std::fs::create_dir(&project).unwrap();
    // This test binary owns its isolated config and never loads the user's hosts.
    unsafe {
        std::env::set_var("TETHER_CONFIG_DIR", config.path());
    }
    std::fs::write(config.path().join("settings.json"), r#"{"updates":{"check_on_startup":false},"agents":{"launch_shells":{"claude":"powershell"}}}"#)
        .unwrap();
    let mut p = tether::profile::Profile::parse_quick("deploy@192.0.2.10:2222").unwrap();
    p.name = "Example machine".into();
    std::fs::write(config.path().join("profiles.json"), serde_json::to_string(&[p]).unwrap()).unwrap();
    tether::pty::shells::available();
    let mut h = Harness::builder().with_size(egui::vec2(1280.0, 820.0)).with_max_steps(10_000).wgpu().build_eframe(|cc| TetherApp::new(cc).unwrap());
    h.run_steps(20);
    h.get_by_label("Codex view").click();
    h.run_steps(5);
    assert!(tether::settings::snapshot().layout.views.is_shown(tether::layout::View::Codex));
    h.get_by_label("Codex sessions");
    h.get_by_label("Launch Codex");
    shot(&mut h, "general-codex-view");
    h.get_by_label("Launch Codex").click();
    h.run_steps(5);
    h.get_by_label("Codex · Launch");
    h.get_by_label("Project directory");
    shot(&mut h, "general-project-launch");
    h.get_by_label("Cancel").click();
    h.run_steps(5);
    h.get_by_label("Codex view").click();
    h.run_steps(5);
    for (launcher, title) in [("Agents view", "Claude Code · Custom Boot"), ("Codex view", "Codex · Custom Boot")] {
        h.get_by_label(launcher).click_secondary();
        h.run_steps(5);
        h.get_by_label("Custom Boot…").click();
        h.run_steps(5);
        h.get_by_label(title);
        if launcher == "Agents view" {
            replace_command(&mut h, "claude -p");
            replace_field(&mut h, 1, &project.display().to_string());
            replace_field(&mut h, 2, "PrismaCore");
            h.get_by_label("☆ Save").click();
            h.run_steps(5);
            assert!(tether::settings::snapshot().projects.entries.iter().any(|p| p.saved && p.name == "PrismaCore"));
            let stored: tether::settings::Settings = serde_json::from_slice(&std::fs::read(config.path().join("settings.json")).unwrap()).unwrap();
            assert!(stored.projects.entries.iter().any(|p| p.saved && p.name == "PrismaCore"));
            assert!(h.query_by_label("Initial prompt").is_none());
            h.get_by_label("Terminal shell");
            shot(&mut h, "general-account-flag");
        }
        h.get_by_label("★  PrismaCore").click();
        h.run_steps(3);
        h.get_all_by_role(egui::accesskit::Role::TextInput).next().unwrap().focus();
        h.key_press_modifiers(Modifiers { ctrl: true, command: true, ..Modifiers::NONE }, Key::A);
        h.event(egui::Event::Text("echo custom_boot_ok".into()));
        h.run_steps(3);
        shot(&mut h, if launcher == "Agents view" { "general-claude-boot" } else { "general-codex-boot" });
        h.get_by_label("Start terminal").click();
        h.run_steps(15);
        assert!(h.query_by_label(title).is_none());
    }
    h.get_by_label("App menu").click();
    h.run_steps(5);
    h.get_by_label_contains("Import / export setup").click();
    h.run_steps(5);
    shot(&mut h, "general-import");
    h.get_by_label("Export setup").click();
    h.run_steps(5);
    h.get_by_label_contains("Anyone with this file");
    h.get_by_label("I understand the file contains connection details");
    shot(&mut h, "general-export");
    h.get_by_label("I understand the file contains connection details").click();
    h.run_steps(5);
    shot(&mut h, "general-export-acknowledged");
    h.get_by_label("Close").click();
    h.run_steps(5);
    h.get_by_label("App menu").click();
    h.run_steps(5);
    h.get_by_label_contains("Key manager").click();
    h.run_steps(5);
    h.get_by_label("Search this PC for SSH keys…").click();
    h.run_steps(5);
    h.get_by_label("Find an SSH key on this PC");
    h.get_by_label("Cancel").click();
    h.run_steps(5);
    h.get_by_label("SSH keys");
    h.run_steps(5);
}

fn shot(h: &mut Harness<'_, TetherApp>, name: &str) {
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ui-shots");
    std::fs::create_dir_all(&output).unwrap();
    h.render().unwrap().save(output.join(format!("{name}.png"))).unwrap();
}

fn replace_command(h: &mut Harness<'_, TetherApp>, text: &str) {
    replace_field(h, 0, text);
}

fn replace_field(h: &mut Harness<'_, TetherApp>, index: usize, text: &str) {
    h.get_all_by_role(egui::accesskit::Role::TextInput).nth(index).unwrap().focus();
    h.key_press_modifiers(Modifiers { ctrl: true, command: true, ..Modifiers::NONE }, Key::A);
    h.event(egui::Event::Text(text.into()));
    h.run_steps(5);
}
