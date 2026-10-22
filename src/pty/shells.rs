//! Local shells tether can start: PowerShell 7, Windows PowerShell, cmd, Git Bash and
//! WSL distros on Windows; `$SHELL` plus bash/zsh/fish elsewhere.
//!
//! Detection runs once (WSL needs a `wsl.exe` round trip, capped at [`WSL_TIMEOUT`]) and
//! is cached; `warm_up` starts it in the background at launch. UI code reads
//! [`detected`], which never blocks (`None` while detection is still running).

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

/// How long `wsl.exe -l -q` may take before it is killed and WSL is skipped.
pub const WSL_TIMEOUT: Duration = Duration::from_secs(2);

/// Picks the glyph and accent of a shell in menus and tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellIcon {
    PowerShell,
    Cmd,
    GitBash,
    Wsl,
    Unix,
}

impl ShellIcon {
    pub fn glyph(self) -> &'static str {
        match self {
            ShellIcon::PowerShell => "»",
            ShellIcon::Cmd => ">",
            ShellIcon::GitBash => "$",
            ShellIcon::Wsl => "🐧",
            ShellIcon::Unix => "$",
        }
    }

    /// Accent hue (0..360) for the tab marker, like a host's colour.
    pub fn hue(self) -> f32 {
        match self {
            ShellIcon::PowerShell => 210.0,
            ShellIcon::Cmd => 45.0,
            ShellIcon::GitBash => 12.0,
            ShellIcon::Wsl => 25.0,
            ShellIcon::Unix => 140.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSpec {
    /// Stable id saved in workspaces and settings (`pwsh`, `cmd`, `wsl:Ubuntu`, `zsh`…).
    pub id: String,
    pub name: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub icon_kind: ShellIcon,
}

impl ShellSpec {
    /// A compact name for tab titles: `PowerShell`, `pwsh 7`, `cmd`, `Git Bash`, `Ubuntu`,
    /// `zsh` (menus keep the full [`Self::name`]).
    pub fn short_name(&self) -> String {
        match self.id.as_str() {
            "powershell" => "PowerShell".into(),
            "cmd" => "cmd".into(),
            "pwsh" => {
                // `…\PowerShell\7\pwsh.exe` -> "pwsh 7".
                let version =
                    self.program.parent().and_then(|d| d.file_name()).and_then(|v| v.to_str()).filter(|v| v.starts_with(|c: char| c.is_ascii_digit()));
                version.map_or_else(|| "pwsh".into(), |v| format!("pwsh {v}"))
            }
            id => id.strip_prefix("wsl:").map_or_else(|| self.name.clone(), str::to_string),
        }
    }

    fn new(id: impl Into<String>, name: impl Into<String>, program: impl Into<PathBuf>, args: &[&str], icon_kind: ShellIcon) -> Self {
        Self { id: id.into(), name: name.into(), program: program.into(), args: args.iter().map(|a| a.to_string()).collect(), icon_kind }
    }
}

static DETECTED: OnceLock<Vec<ShellSpec>> = OnceLock::new();

/// Shells found on this machine, in menu order (the first one is the default default).
/// Blocks until detection is done: UI paint code uses [`detected`] instead.
pub fn available() -> &'static [ShellSpec] {
    DETECTED.get_or_init(detect)
}

/// The detected shells, or `None` while detection is still running. Never blocks.
pub fn detected() -> Option<&'static [ShellSpec]> {
    DETECTED.get().map(Vec::as_slice)
}

/// What menus show while [`detected`] is still `None`.
pub const DETECTING: &str = "Detecting shells…";

/// Starts detection on a background thread so the first menu doesn't block on `wsl.exe`;
/// `on_ready` runs once it is done (to repaint menus showing [`DETECTING`]).
pub fn warm_up(on_ready: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name("tether-shells".into()).spawn(|| {
        available();
        on_ready();
    });
}

/// Blocks until detection is done (see [`find_detected`]).
pub fn find(id: &str) -> Option<&'static ShellSpec> {
    available().iter().find(|s| s.id == id)
}

/// Like [`find`], but `None` while detection is still running.
pub fn find_detected(id: &str) -> Option<&'static ShellSpec> {
    detected()?.iter().find(|s| s.id == id)
}

/// The shell from `settings.terminal.default_shell`, else the first one detected.
pub fn default_shell() -> Option<&'static ShellSpec> {
    let wanted = crate::settings::get().terminal.default_shell;
    pick_default(available(), &wanted)
}

pub fn pick_default<'a>(shells: &'a [ShellSpec], wanted: &str) -> Option<&'a ShellSpec> {
    shells.iter().find(|s| !wanted.is_empty() && s.id == wanted).or_else(|| shells.first())
}

/// Distro names from `wsl.exe -l -q`. wsl.exe writes UTF-16LE (with or without a BOM);
/// `WSL_UTF8=1` switches it to UTF-8, so both are accepted. Docker Desktop's internal
/// distros are not shells and are skipped.
pub fn parse_wsl_list(bytes: &[u8]) -> Vec<String> {
    let looks_utf16 = bytes.len() >= 2 && (bytes.starts_with(&[0xFF, 0xFE]) || bytes.iter().skip(1).step_by(2).take(8).all(|b| *b == 0));
    let text = if looks_utf16 {
        let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    text.lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}' || c == '\0'))
        .filter(|l| !l.is_empty() && !l.starts_with("docker-desktop"))
        .map(str::to_string)
        .collect()
}

/// Shell paths from `/etc/shells` (comments and blanks dropped).
pub fn parse_etc_shells(text: &str) -> Vec<PathBuf> {
    text.lines().map(str::trim).filter(|l| l.starts_with('/')).map(PathBuf::from).collect()
}

/// Newest `PowerShell\<version>\pwsh.exe` under a Program Files directory.
pub fn newest_pwsh(dirs: &[PathBuf]) -> Option<PathBuf> {
    let mut found: Vec<(Vec<u32>, PathBuf)> = dirs
        .iter()
        .filter_map(|d| {
            let exe = d.join("pwsh.exe");
            let name = d.file_name()?.to_str()?;
            let version: Vec<u32> = name.split(['.', '-']).map_while(|p| p.parse().ok()).collect();
            (!version.is_empty() && exe.is_file()).then_some((version, exe))
        })
        .collect();
    found.sort();
    found.pop().map(|(_, exe)| exe)
}

fn on_path(exe: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(exe)).find(|p| p.is_file())
}

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from).filter(|p| !p.as_os_str().is_empty())
}

fn first_file(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|p| p.is_file())
}

#[cfg(windows)]
fn detect() -> Vec<ShellSpec> {
    let mut out = Vec::new();
    let program_files: Vec<PathBuf> = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"].iter().filter_map(|v| env_dir(v)).collect();
    let installed_pwsh = || {
        let dirs: Vec<PathBuf> = program_files
            .iter()
            .filter_map(|pf| std::fs::read_dir(pf.join("PowerShell")).ok())
            .flat_map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())))
            .collect();
        newest_pwsh(&dirs)
    };
    // An installed copy under Program Files beats whatever `pwsh.exe` comes first on PATH.
    if let Some(pwsh) = installed_pwsh().or_else(|| on_path("pwsh.exe")) {
        out.push(ShellSpec::new("pwsh", "PowerShell", pwsh, &["-NoLogo"], ShellIcon::PowerShell));
    }
    let system_root = env_dir("SystemRoot").unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let system32 = system_root.join("System32");
    if let Some(ps) = first_file([system32.join(r"WindowsPowerShell\v1.0\powershell.exe")]) {
        out.push(ShellSpec::new("powershell", "Windows PowerShell", ps, &["-NoLogo"], ShellIcon::PowerShell));
    }
    if let Some(cmd) = first_file(env_dir("ComSpec").into_iter().chain([system32.join("cmd.exe")])) {
        out.push(ShellSpec::new("cmd", "Command Prompt", cmd, &[], ShellIcon::Cmd));
    }
    let git_roots =
        git_install_dir().into_iter().chain(program_files.iter().map(|pf| pf.join("Git"))).chain(env_dir("LOCALAPPDATA").map(|d| d.join(r"Programs\Git")));
    if let Some(bash) = first_file(git_roots.map(|root| root.join(r"bin\bash.exe"))) {
        out.push(ShellSpec::new("git-bash", "Git Bash", bash, &["--login", "-i"], ShellIcon::GitBash));
    }
    let wsl = system32.join("wsl.exe");
    if wsl.is_file() {
        for distro in wsl_distros(&wsl) {
            out.push(ShellSpec::new(format!("wsl:{distro}"), format!("{distro} (WSL)"), &wsl, &["-d", &distro, "--cd", "~"], ShellIcon::Wsl));
        }
    }
    out
}

/// Git for Windows records its install directory in the registry.
#[cfg(windows)]
fn git_install_dir() -> Option<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER].into_iter().find_map(|hive| {
        let key = RegKey::predef(hive).open_subkey(r"SOFTWARE\GitForWindows").ok()?;
        key.get_value::<String, _>("InstallPath").ok().map(PathBuf::from)
    })
}

#[cfg(windows)]
fn wsl_distros(wsl: &std::path::Path) -> Vec<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = std::process::Command::new(wsl);
    cmd.args(["-l", "-q"]).creation_flags(CREATE_NO_WINDOW);
    stdout_within(cmd, WSL_TIMEOUT).map(|out| parse_wsl_list(&out)).unwrap_or_default()
}

/// Runs `cmd` and returns its stdout if it exits successfully within `timeout`;
/// otherwise kills it and returns `None` (a hung `wsl.exe` must not hang detection).
pub fn stdout_within(mut cmd: std::process::Command, timeout: Duration) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    // Read on a side thread: a grandchild keeping the pipe open can't block us.
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = rx.recv_timeout(Duration::from_millis(200)).ok()?;
    status.filter(|s| s.success()).map(|_| out)
}

#[cfg(not(windows))]
fn detect() -> Vec<ShellSpec> {
    let mut paths: Vec<PathBuf> = env_dir("SHELL").into_iter().collect();
    let listed = std::fs::read_to_string("/etc/shells").map(|t| parse_etc_shells(&t)).unwrap_or_default();
    for name in ["bash", "zsh", "fish"] {
        let found = listed.iter().find(|p| p.file_name().is_some_and(|f| f == name)).cloned().or_else(|| on_path(name));
        paths.extend(found);
    }
    let mut out: Vec<ShellSpec> = Vec::new();
    for path in paths {
        let Some(name) = path.file_name().and_then(|f| f.to_str()).map(str::to_string) else { continue };
        if out.iter().any(|s| s.id == name) || !path.is_file() {
            continue;
        }
        out.push(ShellSpec::new(name.clone(), name, path, &["-l"], ShellIcon::Unix));
    }
    if out.is_empty()
        && let Some(sh) = first_file([PathBuf::from("/bin/sh")])
    {
        out.push(ShellSpec::new("sh", "sh", sh, &[], ShellIcon::Unix));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(text: &str, bom: bool) -> Vec<u8> {
        let mut out = if bom { vec![0xFF, 0xFE] } else { Vec::new() };
        out.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        out
    }

    #[test]
    fn parses_utf16_wsl_list_with_and_without_bom() {
        let raw = "Ubuntu\r\nDebian\r\ndocker-desktop\r\ndocker-desktop-data\r\n\r\n";
        assert_eq!(parse_wsl_list(&utf16(raw, false)), vec!["Ubuntu", "Debian"]);
        assert_eq!(parse_wsl_list(&utf16(raw, true)), vec!["Ubuntu", "Debian"]);
    }

    #[test]
    fn parses_utf8_wsl_list_and_empty_output() {
        assert_eq!(parse_wsl_list(b"Ubuntu-22.04\nkali-linux\n"), vec!["Ubuntu-22.04", "kali-linux"]);
        assert!(parse_wsl_list(&[]).is_empty());
        assert!(parse_wsl_list(&utf16("\r\n", false)).is_empty());
    }

    #[test]
    fn parses_etc_shells() {
        let text = "# /etc/shells: valid login shells\n/bin/sh\n/usr/bin/zsh\n\n  /usr/bin/fish  \nnot-a-path\n";
        assert_eq!(parse_etc_shells(text), vec![PathBuf::from("/bin/sh"), PathBuf::from("/usr/bin/zsh"), PathBuf::from("/usr/bin/fish")]);
    }

    #[test]
    fn newest_pwsh_prefers_highest_version() {
        let dir = tempfile::tempdir().unwrap();
        let mk = |v: &str| {
            let d = dir.path().join(v);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("pwsh.exe"), b"").unwrap();
            d
        };
        let dirs = vec![mk("7"), mk("7-preview"), mk("10"), dir.path().join("6")];
        assert_eq!(newest_pwsh(&dirs), Some(dir.path().join("10").join("pwsh.exe")));
        assert_eq!(newest_pwsh(&[]), None);
    }

    #[test]
    fn short_names_for_tabs() {
        let spec = |id: &str, name: &str, program: &str| ShellSpec::new(id, name, program, &[], ShellIcon::PowerShell);
        assert_eq!(spec("powershell", "Windows PowerShell", "powershell.exe").short_name(), "PowerShell");
        assert_eq!(spec("pwsh", "PowerShell", r"C:\Program Files\PowerShell\7\pwsh.exe").short_name(), "pwsh 7");
        assert_eq!(spec("pwsh", "PowerShell", r"C:\Users\me\AppData\Local\Microsoft\WindowsApps\pwsh.exe").short_name(), "pwsh");
        assert_eq!(spec("wsl:Ubuntu-22.04", "Ubuntu-22.04 (WSL)", "wsl.exe").short_name(), "Ubuntu-22.04");
        assert_eq!(spec("cmd", "Command Prompt", "cmd.exe").short_name(), "cmd");
        assert_eq!(spec("git-bash", "Git Bash", "bash.exe").short_name(), "Git Bash");
    }

    #[test]
    fn default_falls_back_to_first_shell() {
        let shells = vec![
            ShellSpec::new("pwsh", "PowerShell", "pwsh.exe", &[], ShellIcon::PowerShell),
            ShellSpec::new("cmd", "Command Prompt", "cmd.exe", &[], ShellIcon::Cmd),
        ];
        assert_eq!(pick_default(&shells, "cmd").map(|s| s.id.as_str()), Some("cmd"));
        assert_eq!(pick_default(&shells, "").map(|s| s.id.as_str()), Some("pwsh"));
        assert_eq!(pick_default(&shells, "wsl:Gone").map(|s| s.id.as_str()), Some("pwsh"));
        assert!(pick_default(&[], "cmd").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn slow_commands_are_killed_at_the_timeout() {
        let mut slow = std::process::Command::new("cmd");
        slow.args(["/C", "ping -n 6 127.0.0.1 >NUL"]);
        let started = std::time::Instant::now();
        assert_eq!(stdout_within(slow, Duration::from_millis(300)), None);
        assert!(started.elapsed() < Duration::from_secs(2), "took {:?}", started.elapsed());
        let mut fast = std::process::Command::new("cmd");
        fast.args(["/C", "echo hi"]);
        let out = stdout_within(fast, Duration::from_secs(5)).map(|o| String::from_utf8_lossy(&o).trim().to_string());
        assert_eq!(out.as_deref(), Some("hi"));
    }

    #[test]
    fn detects_at_least_one_shell() {
        assert!(!available().is_empty());
        let first = &available()[0];
        assert!(find(&first.id).is_some());
        assert!(detected().is_some_and(|d| !d.is_empty()));
    }
}
