//! "New → Claude Code / Codex": agents found on `PATH`, started by typing their command
//! into a fresh default shell (so the user lands in the shell when the agent exits).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::AgentKind;

#[derive(Debug, Clone)]
pub struct Launcher {
    pub kind: AgentKind,
    /// What is typed into the shell.
    pub command: &'static str,
    pub path: PathBuf,
}

const CANDIDATES: [(AgentKind, &str); 4] =
    [(AgentKind::Claude, "claude"), (AgentKind::Codex, "codex"), (AgentKind::Gemini, "gemini"), (AgentKind::OpenCode, "opencode")];

static FOUND: OnceLock<Vec<Launcher>> = OnceLock::new();

/// Agents installed on this machine (searched once).
pub fn available() -> &'static [Launcher] {
    FOUND.get_or_init(|| {
        let dirs = crate::platform::executable_dirs();
        let exts = executable_extensions();
        CANDIDATES.iter().filter_map(|(kind, cmd)| find(dirs, cmd, &exts).map(|path| Launcher { kind: *kind, command: cmd, path })).collect()
    })
}

/// The agents found, or `None` while the `PATH` scan is still running. Never blocks.
pub fn found() -> Option<&'static [Launcher]> {
    FOUND.get().map(Vec::as_slice)
}

/// Searches `PATH` on a background thread so the first "+" menu does not wait.
pub fn warm_up() {
    let _ = std::thread::Builder::new().name("tether-agent-scan".into()).spawn(|| {
        available();
    });
}

fn executable_extensions() -> Vec<String> {
    if !cfg!(windows) {
        return vec![String::new()];
    }
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    pathext.split(';').filter(|e| !e.is_empty()).map(|e| e.to_ascii_lowercase()).collect()
}

pub fn find(dirs: &[PathBuf], name: &str, exts: &[String]) -> Option<PathBuf> {
    dirs.iter().flat_map(|d| exts.iter().map(move |e| d.join(format!("{name}{e}")))).find(|p| is_file(p))
}

fn is_file(p: &Path) -> bool {
    p.metadata().is_ok_and(|m| m.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_commands_with_extensions_in_path_order() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(b.path().join("claude.cmd"), "").unwrap();
        std::fs::write(a.path().join("codex"), "").unwrap();
        std::fs::create_dir(a.path().join("claude.exe")).unwrap();
        let dirs = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        let exts = vec![".exe".to_string(), ".cmd".to_string()];
        assert_eq!(find(&dirs, "claude", &exts), Some(b.path().join("claude.cmd")), "a folder named claude.exe is not a program");
        assert_eq!(find(&dirs, "codex", &[String::new()]), Some(a.path().join("codex")));
        assert_eq!(find(&dirs, "gemini", &exts), None);
    }
}
