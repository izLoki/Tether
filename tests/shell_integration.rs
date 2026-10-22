//! Starts a real local shell with tether's shell integration and checks the command
//! blocks it reports: exit codes, command text and working directory.

use std::time::{Duration, Instant};

use tether::pty::run_local_pty;
use tether::pty::shells::{ShellIcon, ShellSpec};
use tether::shell_integration::blocks::{Block, Phase};
use tether::ssh::terminal::{LinkState, TerminalHandle};

const TIMEOUT: Duration = Duration::from_secs(60);

#[cfg(windows)]
fn shell() -> ShellSpec {
    let found = tether::pty::shells::available().iter().find(|s| s.id == "pwsh").or_else(|| tether::pty::shells::find("powershell"));
    found.cloned().unwrap_or_else(|| {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        ShellSpec {
            id: "powershell".into(),
            name: "Windows PowerShell".into(),
            program: format!(r"{root}\System32\WindowsPowerShell\v1.0\powershell.exe").into(),
            args: vec!["-NoLogo".into()],
            icon_kind: ShellIcon::PowerShell,
        }
    })
}

#[cfg(unix)]
fn shell() -> ShellSpec {
    ShellSpec { id: "bash".into(), name: "bash".into(), program: "bash".into(), args: vec!["-l".into()], icon_kind: ShellIcon::Unix }
}

/// Fails the command with exit code 3.
#[cfg(windows)]
const FAIL_3: &str = "cmd /c exit 3";
#[cfg(unix)]
const FAIL_3: &str = "sh -c 'exit 3'";

fn screen_text(h: &TerminalHandle) -> String {
    h.shared.lock().map(|s| s.parser.screen().contents()).unwrap_or_default()
}

fn blocks(h: &TerminalHandle) -> Vec<Block> {
    h.shared.lock().map(|s| s.parser.callbacks().shell.blocks().iter().cloned().collect()).unwrap_or_default()
}

fn wait_for(h: &TerminalHandle, what: &str, done: impl Fn(&[Block]) -> bool) {
    let start = Instant::now();
    while !done(&blocks(h)) {
        assert!(start.elapsed() < TIMEOUT, "timed out waiting for {what}; blocks: {:#?}\nscreen:\n{}", blocks(h), screen_text(h));
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn done(blocks: &[Block]) -> Vec<&Block> {
    blocks.iter().filter(|b| b.phase == Phase::Done).collect()
}

#[test]
fn local_shell_reports_blocks_with_exit_codes_and_cwd() {
    scenario(shell(), FAIL_3);
}

#[test]
fn shift_enter_inserts_a_line_and_clear_removes_old_blocks() {
    let (handle, rx) = TerminalHandle::new(30, 120);
    let spec = shell();
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();
    let h = handle.clone();
    let runner = std::thread::spawn(move || run_local_pty(&spec, Some(cwd), h, rx, |_| {}));
    wait_for(&handle, "prompt", |b| b.last().is_some_and(|b| b.phase == Phase::Prompt));
    handle.send(b"echo first_line".to_vec());
    let shifted = tether::ui::keys::encode(eframe::egui::Key::Enter, eframe::egui::Modifiers::SHIFT, false).unwrap();
    handle.send(shifted);
    std::thread::sleep(Duration::from_millis(500));
    assert!(done(&blocks(&handle)).is_empty(), "Shift+Enter submitted the command");
    handle.send(b"echo second_line\r".to_vec());
    wait_for(&handle, "multiline command", |b| !done(b).is_empty());
    let command = blocks(&handle).into_iter().find(|b| b.phase == Phase::Done).unwrap().command.unwrap();
    assert!(command.contains("first_line") && command.contains("second_line") && command.contains('\n'), "{command:?}");
    #[cfg(windows)]
    handle.send(b"cls\r".to_vec());
    #[cfg(unix)]
    handle.send(b"clear\r".to_vec());
    wait_for(&handle, "clear to erase old decorations", |b| b.iter().all(|b| b.command.as_deref().is_none_or(|c| !c.contains("first_line"))));
    handle.close();
    runner.join().unwrap().unwrap();
}

/// Git Bash through ConPTY (skipped when Git for Windows is not installed).
#[cfg(windows)]
#[test]
fn git_bash_reports_blocks_with_exit_codes_and_cwd() {
    if let Some(spec) = tether::pty::shells::find("git-bash") {
        scenario(spec.clone(), "sh -c 'exit 3'");
    }
}

/// Runs a failing command and `echo hi`, then checks both blocks.
fn scenario(spec: ShellSpec, fail_3: &str) {
    let (handle, rx) = TerminalHandle::new(30, 120);
    let cwd = tempfile::tempdir().unwrap();
    let dir = cwd.path().to_path_buf();
    let h = handle.clone();
    let runner = std::thread::spawn(move || run_local_pty(&spec, Some(dir), h, rx, |_| {}));

    wait_for(&handle, "the first prompt mark", |b| b.last().is_some_and(|b| b.phase == Phase::Prompt));
    // A block is complete once the next prompt sits below its output (on Windows the
    // marks can arrive a frame before the text they follow).
    let settled = |b: &[Block], n: usize, output_rows: u64| {
        let finished = done(b);
        finished.len() >= n
            && b.last().is_some_and(|last| last.phase == Phase::Prompt)
            && finished[n - 1].output_range().is_some_and(|(from, to)| to.unwrap_or(0) >= from + output_rows)
    };
    handle.send(format!("{fail_3}\r").into_bytes());
    wait_for(&handle, "the failing command", |b| settled(b, 1, 0));
    handle.send(b"echo hi\r".to_vec());
    wait_for(&handle, "echo", |b| settled(b, 2, 1));

    let all = blocks(&handle);
    let finished = done(&all);
    assert_eq!(finished.len(), 2, "{all:#?}");
    let (fail, echo) = (finished[0], finished[1]);
    assert_eq!(fail.exit, Some(3), "{fail:#?}");
    assert_eq!(fail.command.as_deref(), Some(fail_3));
    assert_eq!(echo.exit, Some(0), "{echo:#?}");
    assert_eq!(echo.command.as_deref(), Some("echo hi"));
    assert!(fail.duration().is_some() && fail.output_line.is_some() && echo.end_line > echo.output_line, "{echo:#?}");
    // The folder came through OSC 7 (temp paths may be shortened or symlinked).
    let reported = fail.cwd.as_deref().unwrap_or_default().to_lowercase().replace('\\', "/");
    let leaf = cwd.path().file_name().unwrap().to_string_lossy().to_lowercase();
    assert!(reported.ends_with(&leaf), "cwd {reported:?} should end with {leaf:?}");

    // The output of `echo hi` is the text between its marks.
    let text = {
        let mut s = handle.shared.lock().unwrap();
        let map = s.parser.callbacks().shell.line_map();
        map.text(s.parser.screen_mut(), echo.output_line.unwrap(), echo.end_line.unwrap())
    };
    assert_eq!(text.trim(), "hi", "output of echo {echo:#?}; screen:\n{}", screen_text(&handle));

    handle.close();
    let start = Instant::now();
    while !handle.shared.lock().is_ok_and(|s| matches!(s.state, LinkState::Closed(_))) {
        assert!(start.elapsed() < TIMEOUT, "shell did not close");
        std::thread::sleep(Duration::from_millis(25));
    }
    runner.join().unwrap().unwrap();
}
