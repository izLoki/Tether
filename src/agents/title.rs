//! What a tab running an AI agent is called. Claude Code names its session in the
//! window title (`⠐ Refactor auth module` while it works, `✳ Refactor auth module`
//! while it waits); other agents get their name and the project folder.
//!
//! Pure functions: the tab passes in what it knows, so every rule is unit tested.

use super::AgentKind;

/// Leading marks agents put before their title: braille spinner frames and the
/// sparkles Claude Code cycles through.
fn is_status_glyph(c: char) -> bool {
    ('\u{2800}'..='\u{28FF}').contains(&c) || matches!(c, '✳' | '✢' | '✶' | '✻' | '✽' | '·' | '*' | '●' | '○')
}

/// The title without its leading status marks (`⠐ ✳ Fix it` -> `Fix it`).
pub fn strip_status(title: &str) -> &str {
    title.trim_start_matches(|c: char| is_status_glyph(c) || c.is_whitespace()).trim_end()
}

/// Whether `title` was set by an agent: it starts with a spinner frame or Claude's `✳`.
/// Shell titles never do, so a title like this is stale once the agent has quit.
pub fn is_agent_title(title: &str) -> bool {
    title.trim_start().chars().next().is_some_and(|c| ('\u{2800}'..='\u{28FF}').contains(&c) || c == '✳')
}

/// A title that only names the program (`Claude Code`, `claude`), not the task.
fn is_generic(kind: AgentKind, title: &str) -> bool {
    let t = title.trim();
    t.is_empty() || t.eq_ignore_ascii_case(kind.name()) || t.eq_ignore_ascii_case(kind.id()) || (kind == AgentKind::Claude && t.eq_ignore_ascii_case("claude"))
}

/// Last folder of `cwd` (`D:\work\api-server\` -> `api-server`).
pub fn folder_name(cwd: &str) -> Option<&str> {
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    trimmed.rsplit(['/', '\\']).next().filter(|f| !f.is_empty() && !f.ends_with(':'))
}

/// The session's name: the agent's own title when it set one, else
/// `Claude Code · <project folder>` (or just the agent's name without a folder).
pub fn session_title(kind: AgentKind, raw_title: Option<&str>, cwd: Option<&str>) -> String {
    if let Some(raw) = raw_title.filter(|t| is_agent_title(t)) {
        let task = strip_status(raw);
        if !is_generic(kind, task) {
            return task.to_string();
        }
    }
    match cwd.and_then(folder_name) {
        Some(folder) => format!("{} · {folder}", kind.name()),
        None => kind.name().to_string(),
    }
}

/// The title a local terminal tab shows. In order: the user's name for the tab, the
/// agent session's name while an agent runs, else what `shell` makes of the terminal
/// title (a stale agent title is dropped, so the tab reverts once the agent quits).
pub fn tab_title(
    custom: Option<&str>,
    agent: Option<AgentKind>,
    raw_title: Option<&str>,
    cwd: Option<&str>,
    shell: impl FnOnce(Option<&str>) -> String,
) -> String {
    if let Some(name) = custom.map(str::trim).filter(|n| !n.is_empty()) {
        return name.to_string();
    }
    if let Some(kind) = agent {
        return session_title(kind, raw_title, cwd);
    }
    shell(raw_title.filter(|t| !is_agent_title(t)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(t: Option<&str>) -> String {
        t.map_or_else(|| "pwsh 7".to_string(), str::to_string)
    }

    #[test]
    fn spinner_and_sparkle_prefixes_are_stripped() {
        assert_eq!(strip_status("\u{2810} Refactor auth module"), "Refactor auth module");
        assert_eq!(strip_status("✳ Refactor auth module  "), "Refactor auth module");
        assert_eq!(strip_status("⠋✳  Fix"), "Fix");
        assert_eq!(strip_status("plain"), "plain");
        assert!(is_agent_title("⠐ x") && is_agent_title("  ✳ x"));
        assert!(!is_agent_title("pwsh") && !is_agent_title(""));
    }

    #[test]
    fn claude_session_title_comes_from_its_osc_title() {
        let k = AgentKind::Claude;
        assert_eq!(session_title(k, Some("\u{2810} Refactor auth module"), Some("D:/work/api")), "Refactor auth module");
        assert_eq!(session_title(k, Some("✳ Write release notes"), None), "Write release notes");
    }

    #[test]
    fn generic_or_missing_titles_fall_back_to_agent_and_folder() {
        let k = AgentKind::Claude;
        assert_eq!(session_title(k, Some("✳ Claude Code"), Some(r"D:\work\api-server\")), "Claude Code · api-server");
        assert_eq!(session_title(k, Some("✳ "), Some("/home/me/site")), "Claude Code · site");
        assert_eq!(session_title(k, None, None), "Claude Code");
        // A shell's own title is not the agent's task name.
        assert_eq!(session_title(k, Some("C:\\WINDOWS\\system32\\cmd.exe"), Some("C:/src/app")), "Claude Code · app");
        assert_eq!(session_title(AgentKind::Codex, Some("pwsh"), Some("C:/src/app")), "Codex · app");
        assert_eq!(session_title(AgentKind::Codex, Some("⠋ codex"), None), "Codex");
    }

    #[test]
    fn custom_names_win_and_tabs_revert_when_the_agent_quits() {
        let raw = Some("✳ Refactor auth module");
        assert_eq!(tab_title(Some("api work"), Some(AgentKind::Claude), raw, None, shell), "api work");
        assert_eq!(tab_title(Some("  "), Some(AgentKind::Claude), raw, None, shell), "Refactor auth module");
        assert_eq!(tab_title(None, Some(AgentKind::Claude), raw, None, shell), "Refactor auth module");
        // Agent gone: its stale title is ignored and the shell title is back.
        assert_eq!(tab_title(None, None, raw, None, shell), "pwsh 7");
        assert_eq!(tab_title(None, None, Some("vim notes.md"), None, shell), "vim notes.md");
    }

    #[test]
    fn folder_names() {
        assert_eq!(folder_name("D:/work/api/"), Some("api"));
        assert_eq!(folder_name(r"C:\"), None);
        assert_eq!(folder_name("/"), None);
    }
}
