//! Optional local integration: a real Codex CLI on a PTY, process/resource detection,
//! then an official-format completion payload through Tether's hook and IPC adapter.
//! No model prompt is submitted and the CLI uses a disposable CODEX_HOME.
#![cfg(windows)]

use std::time::{Duration, Instant};

use tether::agents::{AgentHub, AgentKind, AgentState, hook, hub::TermInfo, ipc};
use tether::pty::shells::{ShellIcon, ShellSpec};
use tether::ssh::terminal::TerminalHandle;

struct CloseOnDrop(TerminalHandle);
impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[test]
#[ignore = "requires TETHER_CODEX_E2E_BIN pointing to an installed Codex CLI"]
fn real_codex_session_and_completion_notification() {
    let bin = std::env::var_os("TETHER_CODEX_E2E_BIN").expect("Codex CLI executable");
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("codex-home");
    std::fs::create_dir(&home).unwrap();
    // This binary has one test and owns the environment for its child CLI.
    unsafe {
        std::env::set_var("TETHER_CONFIG_DIR", dir.path());
        std::env::set_var("CODEX_HOME", &home);
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let ctx = eframe::egui::Context::default();
    let mut hub = AgentHub::new(rt.handle(), &ctx);
    hub.monitor().set_focused(true);
    hub.monitor().set_interval(Duration::from_millis(300));
    let (handle, rx) = TerminalHandle::new(30, 100);
    let close = CloseOnDrop(handle.clone());
    let spec =
        ShellSpec { id: "codex-e2e".into(), name: "Codex CLI".into(), program: bin.into(), args: vec!["--no-alt-screen".into()], icon_kind: ShellIcon::Unix };
    let child_handle = handle.clone();
    let cwd = dir.path().to_path_buf();
    let runner = std::thread::spawn(move || tether::pty::run_local_pty(&spec, Some(cwd), child_handle, rx, |_| {}));
    let frame = |hub: &mut AgentHub| {
        hub.frame(&ctx, vec![TermInfo { uid: 42, handle: &handle, visible: false, label: "Codex test", custom: None }], Instant::now());
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        frame(&mut hub);
        if hub.sessions().iter().any(|s| s.kind == AgentKind::Codex && s.usage.as_ref().is_some_and(|u| u.mem > 0)) {
            break;
        }
        assert!(Instant::now() < deadline, "Codex was not detected: {}", handle.shared.lock().unwrap().parser.screen().contents());
        std::thread::sleep(Duration::from_millis(50));
    }
    // Simulate the payload Codex supplies to notify, exercising the actual hook sender.
    unsafe {
        std::env::set_var("TETHER_SESSION", &handle.tag);
        std::env::set_var("TETHER_IPC", ipc::endpoint().unwrap());
    }
    let payload =
        serde_json::json!({"type":"agent-turn-complete", "thread-id":"e2e-thread", "cwd":dir.path(), "last-assistant-message":"Completion adapter verified"});
    hook::run(&["codex".into(), payload.to_string()]);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        frame(&mut hub);
        if hub.sessions().iter().any(|s| s.hooked && s.message.as_deref() == Some("Completion adapter verified")) {
            break;
        }
        assert!(Instant::now() < deadline, "completion did not reach the session");
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(hub.sessions()[0].kind, AgentKind::Codex);
    // Startup can still have active children; those hold Working until they finish.
    assert!(matches!(hub.sessions()[0].state, AgentState::Done | AgentState::Working));
    assert!(tether::views::View::Codex.includes_agent(hub.sessions()[0].kind));
    assert!(!tether::views::View::Agents.includes_agent(hub.sessions()[0].kind));
    drop(close);
    runner.join().unwrap().unwrap();
}
