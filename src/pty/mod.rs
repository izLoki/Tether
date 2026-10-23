//! Local terminals: shell detection and a pty driver that feeds the same
//! `TerminalHandle` as an SSH shell.

pub mod local;
pub mod shells;
pub mod tree;

pub use local::run_local_pty;
pub use shells::{ShellIcon, ShellSpec};

use crate::profile::Profile;

/// The stand-in profile a local tab carries (tabs, the connecting screen and detached
/// window titles read their name and colour from it). Its nil id never matches a saved
/// host, so pooled SSH connections and SFTP links ignore local tabs.
pub fn local_profile(spec: &ShellSpec) -> Profile {
    let mut p = Profile::new();
    p.id = uuid::Uuid::nil();
    p.name = spec.name.clone();
    p.host = "localhost".into();
    p.user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default();
    p.hue = spec.icon_kind.hue();
    p
}

/// Tab title of a local shell: its name, plus the folder it reports via OSC 7 or a
/// title it set itself. Titles that are just the shell's own path (cmd and PowerShell
/// set one) say nothing and are dropped.
pub fn tab_title(spec: &ShellSpec, title: Option<&str>, cwd: Option<&str>) -> String {
    let folder = cwd.map(|c| c.trim_end_matches(['/', '\\'])).filter(|c| !c.is_empty()).map(|c| c.rsplit(['/', '\\']).next().unwrap_or(c));
    if let Some(folder) = folder {
        return format!("{} · {folder}", spec.short_name());
    }
    let program = spec.program.to_string_lossy().to_lowercase();
    let stem = spec.program_stem();
    match title.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) if t.to_lowercase() == program || t.to_lowercase().trim_end_matches(".exe").ends_with(&stem) => spec.short_name(),
        Some(t) => t.to_string(),
        None => spec.short_name(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pwsh() -> ShellSpec {
        ShellSpec {
            id: "pwsh".into(),
            name: "PowerShell".into(),
            program: r"C:\Program Files\PowerShell\7\pwsh.exe".into(),
            args: Vec::new(),
            icon_kind: ShellIcon::PowerShell,
        }
    }

    #[test]
    fn titles_prefer_cwd_and_drop_program_paths() {
        let s = pwsh();
        assert_eq!(tab_title(&s, None, None), "pwsh 7");
        assert_eq!(tab_title(&s, Some(r"C:\Program Files\PowerShell\7\pwsh.exe"), None), "pwsh 7");
        assert_eq!(tab_title(&s, Some("vim notes.md"), None), "vim notes.md");
        assert_eq!(tab_title(&s, Some("ignored"), Some("/home/loki/project/")), "pwsh 7 · project");
        assert_eq!(tab_title(&s, None, Some("/")), "pwsh 7");
    }

    #[test]
    fn local_profile_is_never_a_saved_host() {
        let p = local_profile(&pwsh());
        assert!(p.id.is_nil());
        assert_eq!(p.display_name(), "PowerShell");
    }
}
