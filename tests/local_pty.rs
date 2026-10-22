//! Spawns a real local shell through `run_local_pty` and checks its output reaches the
//! vt100 screen, input reaches the shell, and closing ends the session.

use std::time::{Duration, Instant};

use tether::pty::run_local_pty;
use tether::pty::shells::{ShellIcon, ShellSpec};
use tether::ssh::terminal::{LinkState, TerminalHandle};

const TIMEOUT: Duration = Duration::from_secs(20);

#[cfg(windows)]
fn shell() -> ShellSpec {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    ShellSpec {
        id: "cmd".into(),
        name: "Command Prompt".into(),
        program: format!(r"{root}\System32\cmd.exe").into(),
        args: vec!["/Q".into()],
        icon_kind: ShellIcon::Cmd,
    }
}

#[cfg(unix)]
fn shell() -> ShellSpec {
    ShellSpec { id: "sh".into(), name: "sh".into(), program: "/bin/sh".into(), args: Vec::new(), icon_kind: ShellIcon::Unix }
}

fn screen_text(h: &TerminalHandle) -> String {
    h.shared.lock().map(|s| s.parser.screen().contents()).unwrap_or_default()
}

fn wait_for(h: &TerminalHandle, what: &str, done: impl Fn(&TerminalHandle) -> bool) {
    let start = Instant::now();
    while !done(h) {
        assert!(start.elapsed() < TIMEOUT, "timed out waiting for {what}; screen:\n{}", screen_text(h));
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn local_shell_echoes_and_closes() {
    let (handle, rx) = TerminalHandle::new(24, 80);
    let spec = shell();
    let h = handle.clone();
    let runner = std::thread::spawn(move || run_local_pty(&spec, None, h, rx, |_| {}));

    wait_for(&handle, "the shell to start", |h| h.shared.lock().is_ok_and(|s| s.state == LinkState::Connected));
    assert!(handle.shared.lock().unwrap().pid.is_some(), "child pid is recorded");

    handle.send(b"echo tether_ok\r".to_vec());
    // The echoed command line also contains the word: wait for it on a line of its own.
    wait_for(&handle, "echo output", |h| screen_text(h).lines().any(|l| l.trim() == "tether_ok"));

    handle.resize(30, 100);
    handle.close();
    wait_for(&handle, "the session to close", |h| h.shared.lock().is_ok_and(|s| matches!(s.state, LinkState::Closed(_))));
    runner.join().unwrap().unwrap();
}

#[test]
fn shell_exit_reports_its_code() {
    let (handle, rx) = TerminalHandle::new(24, 80);
    let spec = shell();
    let h = handle.clone();
    let runner = std::thread::spawn(move || run_local_pty(&spec, None, h, rx, |_| {}));
    wait_for(&handle, "the shell to start", |h| h.shared.lock().is_ok_and(|s| s.state == LinkState::Connected));
    handle.send(b"exit 3\r".to_vec());
    wait_for(&handle, "the shell to exit", |h| h.shared.lock().is_ok_and(|s| matches!(s.state, LinkState::Closed(_))));
    let state = handle.shared.lock().unwrap().state.clone();
    assert_eq!(state, LinkState::Closed("Process exited with code 3".into()));
    runner.join().unwrap().unwrap();
}

#[test]
#[cfg(windows)]
fn color_capabilities_and_ansi_survive_the_real_pty() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("Project with spaces");
    std::fs::create_dir(&cwd).unwrap();
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let spec = ShellSpec {
        id: "color-probe".into(), name: "Color probe".into(), icon_kind: ShellIcon::Cmd,
        program: format!(r"{root}\System32\WindowsPowerShell\v1.0\powershell.exe").into(),
        args: vec!["-NoLogo".into(), "-NoProfile".into(), "-Command".into(),
            "[Console]::WriteLine('CAPS:'+$env:TERM+':'+$env:COLORTERM+':'+$env:FORCE_COLOR+':'+$env:NO_COLOR); [Console]::WriteLine('CWD:'+(Get-Location).Path); [Console]::WriteLine([char]27+'[31mANSI_RED'+[char]27+'[38;5;202mINDEXED'+[char]27+'[38;2;12;123;234mTRUECOLOR'+[char]27+'[0m')".into()],
    };
    let (handle, rx) = TerminalHandle::new(24, 200);
    run_local_pty(&spec, Some(cwd.clone()), handle.clone(), rx, |_| {}).unwrap();
    let shared = handle.shared.lock().unwrap();
    let screen = shared.parser.screen();
    let text = screen.contents();
    assert!(text.contains("CAPS:xterm-256color:truecolor:3:"), "{text}");
    assert!(!text.contains("CAPS:xterm-256color:truecolor:3:1"), "NO_COLOR leaked from the GUI process");
    assert!(text.contains(&format!("CWD:{}", cwd.display())), "{text}");
    let color_at = |needle: &str| {
        for row in 0..24 {
            let line = screen.contents_between(row, 0, row, 200);
            if let Some(col) = line.find(needle) {
                return screen.cell(row, col as u16).unwrap().fgcolor();
            }
        }
        panic!("missing {needle}: {text}");
    };
    assert_eq!(color_at("ANSI_RED"), vt100::Color::Idx(1));
    assert_eq!(color_at("INDEXED"), vt100::Color::Idx(202));
    assert_eq!(color_at("TRUECOLOR"), vt100::Color::Rgb(12, 123, 234));
}
