//! Shell integration: small scripts loaded into local shells so they report where each
//! prompt and command starts and ends (FinalTerm / VS Code `OSC 133 ; A/B/C/D`), the
//! working directory (`OSC 7`), and under our own number `OSC 6973` the git branch and
//! the command line. [`blocks`] turns those marks into command blocks; `ui::blocks`
//! draws them.
//!
//! How each shell gets its script, keeping the user's own startup files:
//! - **PowerShell** (pwsh 7, Windows PowerShell 5.1): `-NoExit -EncodedCommand <script>`.
//!   The profile loads first, then the script; an encoded command is not a script file,
//!   so the execution policy does not block it and nothing is written to disk.
//! - **bash** (Linux, macOS, Git Bash): `--rcfile <file> -i`, where the file loads what
//!   bash itself would (`/etc/profile` + `~/.bash_profile` for login shells,
//!   `/etc/bash.bashrc` + `~/.bashrc` otherwise) and then the integration.
//! - **zsh**: `ZDOTDIR` points at a folder whose `.zshenv`/`.zprofile`/`.zshrc`/`.zlogin`
//!   each load the user's file of the same name; `.zshrc` then loads the integration.
//! - **fish**: `--init-command "source <file>"` (runs after the user's config).
//! - **WSL**: `wsl.exe … --exec sh <launcher>`: the launcher (shared through `WSLENV`,
//!   which translates the Windows path) starts the distro user's login shell as above.
//! - **cmd.exe**: not supported (its prompt cannot run code); tabs work as before.
//!
//! SSH sessions are never injected into: if the remote shell emits these sequences
//! itself (for example after sourcing `bash.sh` from its rc file), they are parsed the
//! same way. Every script is a no-op unless `TETHER_SESSION` is set and guards against
//! being loaded twice.

pub mod blocks;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::pty::shells::{ShellIcon, ShellSpec};

pub const PWSH: &str = include_str!("pwsh.ps1");
pub const BASH: &str = include_str!("bash.sh");
pub const ZSH: &str = include_str!("zsh.zsh");
pub const FISH: &str = include_str!("fish.fish");

/// `settings.shell_integration`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellIntegrationSettings {
    /// Load the integration scripts into new local shells.
    pub enabled: bool,
    /// Coloured marks next to each prompt (exit status, hover details, block menu).
    pub gutter_marks: bool,
    /// A faint line between command blocks.
    pub separators: bool,
    /// Pin the running/scrolled command at the top while reading its output.
    pub sticky_header: bool,
    /// Notify when a command that ran at least `notify_after_secs` finishes out of sight.
    pub notify_long_commands: bool,
    pub notify_after_secs: u32,
}

impl ShellIntegrationSettings {
    pub const NOTIFY_MIN_SECS: u32 = 2;
    pub const NOTIFY_MAX_SECS: u32 = 600;

    pub fn notify_after(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(self.notify_after_secs.clamp(Self::NOTIFY_MIN_SECS, Self::NOTIFY_MAX_SECS)))
    }
}

impl Default for ShellIntegrationSettings {
    fn default() -> Self {
        Self { enabled: true, gutter_marks: true, separators: true, sticky_header: true, notify_long_commands: true, notify_after_secs: 10 }
    }
}

/// How to start a shell with the integration loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    PowerShell,
    Bash,
    Zsh,
    Fish,
    Wsl,
    Unsupported,
}

pub fn kind(spec: &ShellSpec) -> ShellKind {
    let stem = spec.program_stem();
    match (spec.icon_kind, stem.as_str()) {
        (ShellIcon::Cmd, _) => ShellKind::Unsupported,
        (ShellIcon::Wsl, _) | (_, "wsl") => ShellKind::Wsl,
        (_, "pwsh" | "powershell") => ShellKind::PowerShell,
        (_, "bash") => ShellKind::Bash,
        (_, "zsh") => ShellKind::Zsh,
        (_, "fish") => ShellKind::Fish,
        _ => ShellKind::Unsupported,
    }
}

/// Arguments and extra environment to start `spec` with the integration, or `None` to
/// start it unchanged (unsupported shell, or the scripts could not be written).
pub fn launch(spec: &ShellSpec) -> Option<Launch> {
    let kind = kind(spec);
    if kind == ShellKind::PowerShell {
        return Some(pwsh_launch(&spec.args));
    }
    if kind == ShellKind::Unsupported {
        return None;
    }
    let dir = script_dir()?;
    Some(match kind {
        ShellKind::Bash => bash_launch(&spec.args, dir),
        ShellKind::Zsh => zsh_launch(&spec.args, dir),
        ShellKind::Fish => fish_launch(&spec.args, dir),
        _ => wsl_launch(&spec.args, dir),
    })
}

fn pwsh_launch(args: &[String]) -> Launch {
    let mut args = args.to_vec();
    args.extend(["-NoExit".into(), "-EncodedCommand".into(), encode_command(PWSH)]);
    Launch { args, env: Vec::new() }
}

/// `-EncodedCommand` takes base64 of the UTF-16LE text.
pub fn encode_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&bytes)
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Login flags are replaced by our rc file (bash ignores `--rcfile` in a login shell);
/// the rc file then loads the login files itself.
fn bash_launch(args: &[String], dir: &Path) -> Launch {
    let mut login = false;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-l" | "--login" => login = true,
            "-i" => {}
            "--rcfile" | "--init-file" => {
                it.next();
            }
            _ => rest.push(a.clone()),
        }
    }
    let mut out = vec!["--rcfile".to_string(), shell_path(&dir.join("bash-rc.sh")), "-i".into()];
    out.extend(rest);
    let env = if login { vec![("TETHER_BASH_LOGIN".into(), "1".into())] } else { Vec::new() };
    Launch { args: out, env }
}

fn zsh_launch(args: &[String], dir: &Path) -> Launch {
    let user = std::env::var("ZDOTDIR").ok().filter(|d| !d.is_empty()).or_else(|| std::env::var("HOME").ok()).unwrap_or_default();
    let env = vec![("ZDOTDIR".into(), shell_path(&dir.join("zsh"))), ("TETHER_USER_ZDOTDIR".into(), user)];
    Launch { args: args.to_vec(), env }
}

fn fish_launch(args: &[String], dir: &Path) -> Launch {
    let mut out = args.to_vec();
    out.extend(["--init-command".into(), format!("source {}", fish_quote(&shell_path(&dir.join("fish.fish"))))]);
    Launch { args: out, env: Vec::new() }
}

fn wsl_launch(args: &[String], dir: &Path) -> Launch {
    let mut out = args.to_vec();
    out.extend(["--exec".into(), "sh".into(), "-c".into(), r#"exec sh "$TETHER_SI_DIR/launch.sh""#.into()]);
    let mut wslenv = std::env::var("WSLENV").unwrap_or_default();
    for var in ["TETHER_SESSION/u", "TERM_PROGRAM/u", "TETHER_SI_DIR/p"] {
        if !wslenv.is_empty() {
            wslenv.push(':');
        }
        wslenv.push_str(var);
    }
    Launch { args: out, env: vec![("TETHER_SI_DIR".into(), dir.display().to_string()), ("WSLENV".into(), wslenv)] }
}

/// fish single-quoted string.
fn fish_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// A path as the shell will read it: forward slashes (MSYS bash understands `C:/…`).
fn shell_path(p: &Path) -> String {
    let s = p.display().to_string();
    if cfg!(windows) { s.replace('\\', "/") } else { s }
}

/// Startup file for `bash --rcfile`: what bash would have loaded, then the integration.
const BASH_RC_PRELUDE: &str = r#"# tether: bash startup file (passed with --rcfile). Loads the files bash itself would
# load, then tether's shell integration.
if [ -n "${TETHER_BASH_LOGIN-}" ]; then
    unset TETHER_BASH_LOGIN
    [ -r /etc/profile ] && . /etc/profile
    if [ -r ~/.bash_profile ]; then . ~/.bash_profile
    elif [ -r ~/.bash_login ]; then . ~/.bash_login
    elif [ -r ~/.profile ]; then . ~/.profile
    fi
else
    [ -r /etc/bash.bashrc ] && . /etc/bash.bashrc
    [ -r ~/.bashrc ] && . ~/.bashrc
fi
"#;

/// One of our zsh startup files: loads the user's file of the same name with their
/// `ZDOTDIR`, then puts ours back so zsh keeps reading from here.
fn zsh_file(name: &str, extra: &str) -> String {
    format!(
        r#"# tether: zsh {name} (ZDOTDIR points here). Loads the user's {name}.
__tether_zdotdir=$ZDOTDIR
ZDOTDIR=${{TETHER_USER_ZDOTDIR:-$HOME}}
[[ -r $ZDOTDIR/{name} ]] && . $ZDOTDIR/{name}
TETHER_USER_ZDOTDIR=$ZDOTDIR
ZDOTDIR=$__tether_zdotdir
{extra}"#
    )
}

const ZSH_RESTORE: &str = "ZDOTDIR=$TETHER_USER_ZDOTDIR\nunset __tether_zdotdir TETHER_USER_ZDOTDIR\n";

/// Starts the WSL user's login shell with the integration (see the module docs).
const WSL_LAUNCHER: &str = r#"# tether: starts the user's login shell inside WSL with shell integration.
d=$TETHER_SI_DIR
s=${SHELL-}
[ -n "$s" ] || s=$(getent passwd "$(id -un)" 2>/dev/null | cut -d: -f7)
[ -n "$s" ] || s=/bin/sh
case ${s##*/} in
    bash) export TETHER_BASH_LOGIN=1; exec "$s" --rcfile "$d/bash-rc.sh" -i ;;
    zsh) export TETHER_USER_ZDOTDIR="${ZDOTDIR:-$HOME}" ZDOTDIR="$d/zsh"; exec "$s" -l ;;
    fish) exec "$s" -l --init-command "source '$d/fish.fish'" ;;
    *) exec "$s" -l ;;
esac
"#;

/// Files written to the script folder: (relative path, contents). Line endings are
/// always LF, whatever git checked out.
pub fn files() -> Vec<(&'static str, String)> {
    let lf = |s: &str| s.replace("\r\n", "\n");
    let zshrc = zsh_file(
        ".zshrc",
        &format!("[[ -r ${{ZDOTDIR}}/tether.zsh ]] && . ${{ZDOTDIR}}/tether.zsh\n[[ -o login ]] || {{ {} }}\n", ZSH_RESTORE.replace('\n', "; ")),
    );
    vec![
        ("bash-rc.sh", lf(&format!("{BASH_RC_PRELUDE}\n{BASH}"))),
        ("fish.fish", lf(FISH)),
        ("launch.sh", lf(WSL_LAUNCHER)),
        ("zsh/tether.zsh", lf(ZSH)),
        ("zsh/.zshenv", zsh_file(".zshenv", "")),
        ("zsh/.zprofile", zsh_file(".zprofile", "")),
        ("zsh/.zshrc", zshrc),
        ("zsh/.zlogin", zsh_file(".zlogin", ZSH_RESTORE)),
    ]
}

/// Writes [`files`] once per run under the per-user cache folder (not a shared temp
/// folder another user could plant files in). `None` if that fails.
fn script_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let base = directories::BaseDirs::new()?.cache_dir().join("tether").join("shell-integration");
        write_files(&base).ok().map(|()| base)
    })
    .as_deref()
}

pub fn write_files(dir: &Path) -> std::io::Result<()> {
    for (name, text) in files() {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            std::fs::write(&path, text)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(program: &str, args: &[&str], icon: ShellIcon) -> ShellSpec {
        ShellSpec { id: "x".into(), name: "x".into(), program: program.into(), args: args.iter().map(|a| a.to_string()).collect(), icon_kind: icon }
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // PowerShell's own: [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes('dir'))
        assert_eq!(encode_command("dir"), "ZABpAHIA");
    }

    #[test]
    fn shells_are_classified() {
        assert_eq!(kind(&spec(r"C:\Program Files\PowerShell\7\pwsh.exe", &["-NoLogo"], ShellIcon::PowerShell)), ShellKind::PowerShell);
        assert_eq!(kind(&spec(r"C:\Windows\System32\cmd.exe", &[], ShellIcon::Cmd)), ShellKind::Unsupported);
        assert_eq!(kind(&spec(r"C:\Program Files\Git\bin\bash.exe", &["--login", "-i"], ShellIcon::GitBash)), ShellKind::Bash);
        assert_eq!(kind(&spec(r"C:\Windows\System32\wsl.exe", &["-d", "Ubuntu"], ShellIcon::Wsl)), ShellKind::Wsl);
        assert_eq!(kind(&spec("/usr/bin/zsh", &["-l"], ShellIcon::Unix)), ShellKind::Zsh);
        assert_eq!(kind(&spec("/usr/bin/fish", &["-l"], ShellIcon::Unix)), ShellKind::Fish);
        assert_eq!(kind(&spec("/bin/sh", &[], ShellIcon::Unix)), ShellKind::Unsupported);
        assert!(launch(&spec("/bin/sh", &[], ShellIcon::Unix)).is_none());
    }

    #[test]
    fn bash_login_flags_become_an_rcfile() {
        let l = bash_launch(&["--login".into(), "-i".into()], Path::new("/cache/si"));
        assert_eq!(l.args, ["--rcfile", "/cache/si/bash-rc.sh", "-i"]);
        assert_eq!(l.env, [("TETHER_BASH_LOGIN".to_string(), "1".to_string())]);
        let l = bash_launch(&["--norc".into()], Path::new("/x"));
        assert_eq!(l.args, ["--rcfile", "/x/bash-rc.sh", "-i", "--norc"]);
        assert!(l.env.is_empty());
    }

    #[test]
    fn pwsh_keeps_its_args_and_runs_the_script_after_the_profile() {
        let l = pwsh_launch(&["-NoLogo".into()]);
        assert_eq!(&l.args[..3], ["-NoLogo", "-NoExit", "-EncodedCommand"]);
        assert!(!l.args.iter().any(|a| a.eq_ignore_ascii_case("-NoProfile")));
        // Windows' command line limit is 32767 UTF-16 units.
        assert!(l.args[3].len() < 30_000, "encoded script is {} chars", l.args[3].len());
    }

    #[test]
    fn wsl_shares_the_script_folder_through_wslenv() {
        let l = wsl_launch(&["-d".into(), "Ubuntu".into()], Path::new(r"C:\cache\si"));
        assert_eq!(&l.args[..3], ["-d", "Ubuntu", "--exec"]);
        let wslenv = &l.env.iter().find(|(k, _)| k == "WSLENV").unwrap().1;
        assert!(wslenv.ends_with("TETHER_SESSION/u:TERM_PROGRAM/u:TETHER_SI_DIR/p"), "{wslenv}");
    }

    #[test]
    fn scripts_are_guarded_and_written_with_lf() {
        for (name, text) in files() {
            assert!(!text.contains('\r'), "{name} has CR");
            if !name.contains(".z") && name != "launch.sh" {
                assert!(text.contains("TETHER_SESSION"), "{name} is not guarded");
            }
        }
        assert!(PWSH.contains("TETHER_SESSION") && PWSH.contains("__tether_si"));
        let dir = tempfile::tempdir().unwrap();
        write_files(dir.path()).unwrap();
        assert!(dir.path().join("zsh/.zshrc").is_file());
        // Unchanged files are not rewritten.
        write_files(dir.path()).unwrap();
    }

    #[test]
    fn fish_quotes_paths() {
        assert_eq!(fish_quote("/a b/it's"), r"'/a b/it\'s'");
    }

    const ESCAPE_SAMPLE: &str = "a;b\\c\td\u{1b}e ü \\x41";

    fn bash_program() -> Option<PathBuf> {
        if cfg!(windows) {
            // Not System32\bash.exe (that one is WSL).
            crate::pty::shells::available().iter().find(|s| s.id == "git-bash").map(|s| s.program.clone())
        } else {
            Some("bash".into())
        }
    }

    /// The bash script's escaping matches [`blocks::escape`] (skipped without bash).
    #[test]
    fn bash_escape_matches_rust() {
        let Some(bash) = bash_program() else { return };
        let script = format!("{}\n__tether_escape \"$TETHER_TEST_TEXT\"; printf '%s' \"$__tether_out\"", BASH.replace("\r\n", "\n"));
        let out = std::process::Command::new(bash)
            .args(["--norc", "--noprofile", "-i", "-c", &script])
            .env("TETHER_SESSION", "t")
            .env("TETHER_TEST_TEXT", ESCAPE_SAMPLE)
            .output();
        let Ok(out) = out else { return };
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout), blocks::escape(ESCAPE_SAMPLE));
    }

    /// The PowerShell script's escaping matches [`blocks::escape`] (Windows PowerShell).
    #[cfg(windows)]
    #[test]
    fn pwsh_escape_matches_rust() {
        let script = format!("[Console]::OutputEncoding = [Text.Encoding]::UTF8\n{PWSH}\n[Console]::Out.Write((__tether_escape $env:TETHER_TEST_TEXT))");
        let out = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encode_command(&script)])
            .env("TETHER_SESSION", "t")
            .env("TETHER_TEST_TEXT", ESCAPE_SAMPLE)
            .output();
        let Ok(out) = out else { return };
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout), blocks::escape(ESCAPE_SAMPLE));
    }
}
