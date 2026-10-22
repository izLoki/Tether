//! Installing (and removing) our hook entries in the agents' own config files:
//! `~/.claude/settings.json` (Claude Code hooks) and `~/.codex/config.toml` (`notify`).
//!
//! Merges are non-destructive and idempotent: unrelated keys and hooks stay as they are,
//! running install twice changes nothing, and uninstall removes only entries whose command
//! is ours (recognised by its exact form, see [`is_our_claude_command`]). Before writing,
//! the old file is copied to `<name>.tether-bak` (a timestamped name when that exists).

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

/// Claude Code hook events we listen to (`PreToolUse` would fire on every tool call).
pub const CLAUDE_EVENTS: [&str; 5] = ["SessionStart", "UserPromptSubmit", "Notification", "Stop", "SubagentStop"];
/// Seconds Claude Code may wait for our hook (it returns within 300 ms anyway).
const HOOK_TIMEOUT_SECS: u64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Claude,
    Codex,
}

impl Target {
    pub fn name(self) -> &'static str {
        match self {
            Target::Claude => "Claude Code",
            Target::Codex => "Codex",
        }
    }

    /// The config file, honouring `CLAUDE_CONFIG_DIR` / `CODEX_HOME`.
    pub fn path(self) -> Option<PathBuf> {
        let home = || directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf());
        match self {
            Target::Claude => {
                std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).or_else(|| home().map(|h| h.join(".claude"))).map(|d| d.join("settings.json"))
            }
            Target::Codex => std::env::var_os("CODEX_HOME").map(PathBuf::from).or_else(|| home().map(|h| h.join(".codex"))).map(|d| d.join("config.toml")),
        }
    }
}

/// `"C:/Program Files/tether/tether.exe"`: forward slashes work in cmd, PowerShell and
/// the Git Bash Claude Code runs hooks with, and need no escaping in either.
fn exe_arg(exe: &Path) -> String {
    exe.to_string_lossy().replace('\\', "/")
}

/// The exe path as it goes into a shell command line, refused when it holds characters
/// a shell would interpret inside double quotes (or that end them).
fn safe_exe_arg(exe: &Path) -> Result<String, String> {
    let arg = exe_arg(exe);
    if arg.contains(['"', '$', '`', '%']) {
        return Err(format!("tether's path ({arg}) contains a character (\" $ ` %) that is unsafe in a hook command; move tether to a plain folder"));
    }
    Ok(arg)
}

pub fn claude_command(exe: &Path, event: &str) -> String {
    format!("\"{}\" hook claude {event}", exe_arg(exe))
}

/// Whether `path`'s file is tether's executable (`tether`, `tether.exe`).
fn is_tether_exe(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_ascii_lowercase();
    name == "tether" || name == "tether.exe"
}

/// A hook command we wrote: exactly `"<…/tether[.exe]>" hook claude <Event>`.
pub fn is_our_claude_command(cmd: &str) -> bool {
    let Some((path, rest)) = cmd.strip_prefix('"').and_then(|c| c.split_once('"')) else { return false };
    let Some(event) = rest.strip_prefix(" hook claude ") else { return false };
    is_tether_exe(path) && !event.is_empty() && event.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn hook_commands(group: &Value) -> impl Iterator<Item = &str> {
    group.get("hooks").and_then(Value::as_array).into_iter().flatten().filter_map(|h| h.get("command").and_then(Value::as_str))
}

/// `settings` with our hooks added (or their tether path updated).
pub fn merge_claude(settings: &Value, exe: &Path) -> Result<Value, String> {
    safe_exe_arg(exe)?;
    let mut root = match settings {
        Value::Null => Map::new(),
        Value::Object(m) => m.clone(),
        _ => return Err("settings.json is not a JSON object".into()),
    };
    let hooks = root.entry("hooks").or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(hooks) = hooks else { return Err("\"hooks\" in settings.json is not an object".into()) };
    for event in CLAUDE_EVENTS {
        let command = claude_command(exe, event);
        let groups = hooks.entry(event).or_insert_with(|| Value::Array(Vec::new()));
        let Value::Array(groups) = groups else { return Err(format!("\"hooks.{event}\" in settings.json is not a list")) };
        let mut found = false;
        for group in groups.iter_mut() {
            let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) else { continue };
            for hook in list.iter_mut() {
                if hook.get("command").and_then(Value::as_str).is_some_and(is_our_claude_command) {
                    found = true;
                    hook["command"] = Value::String(command.clone());
                }
            }
        }
        if !found {
            groups.push(json!({ "hooks": [{ "type": "command", "command": command, "timeout": HOOK_TIMEOUT_SECS }] }));
        }
    }
    Ok(Value::Object(root))
}

/// `settings` without any hook entry of ours; groups, events and the `hooks` object
/// left empty by that are removed too.
pub fn unmerge_claude(settings: &Value) -> Value {
    let Some(root) = settings.as_object() else { return settings.clone() };
    let mut root = root.clone();
    let Some(Value::Object(hooks)) = root.get_mut("hooks") else { return Value::Object(root) };
    let mut removed_any = false;
    let events: Vec<String> = hooks.keys().cloned().collect();
    for event in events {
        let Some(Value::Array(groups)) = hooks.get_mut(&event) else { continue };
        let mut removed_here = false;
        groups.retain_mut(|group| {
            let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) else { return true };
            let before = list.len();
            list.retain(|h| !h.get("command").and_then(Value::as_str).is_some_and(is_our_claude_command));
            let removed = list.len() != before;
            removed_here |= removed;
            !(removed && list.is_empty())
        });
        if removed_here && groups.is_empty() {
            hooks.shift_remove(&event);
        }
        removed_any |= removed_here;
    }
    if removed_any && hooks.is_empty() {
        root.shift_remove("hooks");
    }
    Value::Object(root)
}

/// Whether every event we need has a hook of ours.
pub fn claude_installed(settings: &Value) -> bool {
    CLAUDE_EVENTS.iter().all(|event| {
        settings
            .pointer(&format!("/hooks/{event}"))
            .and_then(Value::as_array)
            .is_some_and(|groups| groups.iter().flat_map(hook_commands).any(is_our_claude_command))
    })
}

// ---- Codex -------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexMerge {
    /// The new file text.
    Changed(String),
    AlreadyInstalled,
    /// The user has their own `notify`: we leave it alone. Holds it, for the warning.
    Conflict(String),
}

fn codex_notify_is_ours(item: &toml_edit::Item) -> bool {
    let Some(arr) = item.as_array() else { return false };
    let words: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
    matches!(words.as_slice(), [exe, "hook", "codex"] if is_tether_exe(exe))
}

fn codex_notify(exe: &Path) -> toml_edit::Array {
    let mut arr = toml_edit::Array::new();
    for word in [exe_arg(exe).as_str(), "hook", "codex"] {
        arr.push(word);
    }
    arr
}

pub fn merge_codex(text: &str, exe: &Path) -> Result<CodexMerge, String> {
    safe_exe_arg(exe)?;
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("config.toml does not parse: {e}"))?;
    let wanted = codex_notify(exe);
    match doc.get("notify") {
        Some(item) if codex_notify_is_ours(item) => {
            let same = item.as_array().is_some_and(|a| a.iter().map(|v| v.as_str()).eq(wanted.iter().map(|v| v.as_str())));
            if same {
                return Ok(CodexMerge::AlreadyInstalled);
            }
        }
        Some(item) => return Ok(CodexMerge::Conflict(item.to_string().trim().to_string())),
        None => {}
    }
    doc["notify"] = toml_edit::value(wanted);
    Ok(CodexMerge::Changed(doc.to_string()))
}

/// The file without our `notify`, or `None` when it has none of ours.
pub fn unmerge_codex(text: &str) -> Result<Option<String>, String> {
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("config.toml does not parse: {e}"))?;
    if !doc.get("notify").is_some_and(codex_notify_is_ours) {
        return Ok(None);
    }
    doc.remove("notify");
    Ok(Some(doc.to_string()))
}

pub fn codex_installed(text: &str) -> bool {
    text.parse::<toml_edit::DocumentMut>().ok().is_some_and(|d| d.get("notify").is_some_and(codex_notify_is_ours))
}

// ---- plans ------------------------------------------------------------------------------

/// A change to one config file, shown to the user before it is written.
#[derive(Debug, Clone)]
pub struct Plan {
    pub target: Target,
    pub install: bool,
    pub path: PathBuf,
    /// The file as read (`None` if it does not exist yet).
    pub original: Option<String>,
    pub after: String,
    /// Lines of the change: `+` added, `-` removed, ` ` context, `…` skipped.
    pub diff: Vec<(char, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Installed,
    NotInstalled,
    /// Codex already runs another `notify` program.
    Conflict(String),
    Unreadable(String),
}

fn read(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn parse_json(text: Option<&str>) -> Result<Value, String> {
    match text.map(str::trim) {
        None | Some("") => Ok(Value::Null),
        Some(t) => serde_json::from_str(t).map_err(|e| format!("settings.json is not valid JSON ({e}); not touching it")),
    }
}

fn pretty(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap_or_default();
    s.push('\n');
    s
}

pub fn status(target: Target) -> Status {
    let Some(path) = target.path() else { return Status::Unreadable("no home folder".into()) };
    let text = match read(&path) {
        Ok(t) => t,
        Err(e) => return Status::Unreadable(e),
    };
    match target {
        Target::Claude => match parse_json(text.as_deref()) {
            Ok(v) if claude_installed(&v) => Status::Installed,
            Ok(_) => Status::NotInstalled,
            Err(e) => Status::Unreadable(e),
        },
        Target::Codex => {
            let text = text.unwrap_or_default();
            match text.parse::<toml_edit::DocumentMut>() {
                Err(e) => Status::Unreadable(format!("config.toml does not parse: {e}")),
                Ok(d) => match d.get("notify") {
                    Some(item) if codex_notify_is_ours(item) => Status::Installed,
                    Some(item) => Status::Conflict(item.to_string().trim().to_string()),
                    None => Status::NotInstalled,
                },
            }
        }
    }
}

/// What installing (or removing) our entries would write. `Ok(None)`: nothing to do.
pub fn plan(target: Target, install: bool, exe: &Path) -> Result<Option<Plan>, String> {
    let path = target.path().ok_or("no home folder")?;
    let original = read(&path)?;
    let after = match target {
        Target::Claude => {
            let v = parse_json(original.as_deref())?;
            let next = if install { merge_claude(&v, exe)? } else { unmerge_claude(&v) };
            if next == v {
                return Ok(None);
            }
            pretty(&next)
        }
        Target::Codex => {
            let text = original.clone().unwrap_or_default();
            if install {
                match merge_codex(&text, exe)? {
                    CodexMerge::Changed(t) => t,
                    CodexMerge::AlreadyInstalled => return Ok(None),
                    CodexMerge::Conflict(existing) => {
                        return Err(format!("config.toml already has notify = {existing}. tether leaves it alone; remove it first to use tether's."));
                    }
                }
            } else {
                match unmerge_codex(&text)? {
                    Some(t) => t,
                    None => return Ok(None),
                }
            }
        }
    };
    // Compare like with like: a JSON file is rewritten pretty-printed.
    let shown_before = match (target, original.as_deref()) {
        (Target::Claude, Some(t)) => parse_json(Some(t)).map(|v| pretty(&v)).unwrap_or_else(|_| t.to_string()),
        (_, t) => t.unwrap_or_default().to_string(),
    };
    let diff = diff_lines(&shown_before, &after, 2);
    Ok(Some(Plan { target, install, path, original, after, diff }))
}

/// Where the backup of `path` goes: `<name>.tether-bak`, or, when that already exists
/// (the user's first backup must survive later installs), `<name>.tether-bak-<unix time>`.
fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tether-bak");
    let first = path.with_file_name(&name);
    if !first.exists() {
        return first;
    }
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut n = 0u32;
    loop {
        let mut stamped = name.clone();
        stamped.push(if n == 0 { format!("-{secs}") } else { format!("-{secs}-{n}") });
        let candidate = path.with_file_name(stamped);
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

/// Writes `plan` after copying the current file to a backup (see [`backup_path`]).
/// Refuses when the file changed since the plan was made. A symlinked config is written
/// through to its target, and the file keeps its permissions. Returns the backup path,
/// if one was made.
pub fn apply(plan: &Plan) -> Result<Option<PathBuf>, String> {
    let now = read(&plan.path)?;
    if now != plan.original {
        return Err(format!("{} changed in the meantime; review the change again", plan.path.display()));
    }
    // Replace the file a symlink points to, not the link (dotfile managers link these).
    let is_link = std::fs::symlink_metadata(&plan.path).is_ok_and(|m| m.file_type().is_symlink());
    let target =
        if is_link { std::fs::canonicalize(&plan.path).map_err(|e| format!("cannot follow {}: {e}", plan.path.display()))? } else { plan.path.clone() };
    let mut backup = None;
    if let Some(old) = &now {
        let bak = backup_path(&target);
        std::fs::write(&bak, old).map_err(|e| format!("cannot write the backup {}: {e}", bak.display()))?;
        backup = Some(bak);
    }
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let tmp = target.with_extension("tether-tmp");
    std::fs::write(&tmp, &plan.after).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    if let Ok(meta) = std::fs::metadata(&target) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, &target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot replace {}: {e}", target.display())
    })?;
    Ok(backup)
}

/// A line diff (LCS) with `context` unchanged lines around each change.
pub fn diff_lines(before: &str, after: &str, context: usize) -> Vec<(char, String)> {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    let (n, m) = (a.len(), b.len());
    // Config files are small; guard the quadratic table anyway.
    if n * m > 4_000_000 {
        return a.iter().map(|l| ('-', l.to_string())).chain(b.iter().map(|l| ('+', l.to_string()))).collect();
    }
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let mut all = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            all.push((' ', a[i].to_string()));
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            all.push(('+', b[j].to_string()));
            j += 1;
        } else {
            all.push(('-', a[i].to_string()));
            i += 1;
        }
    }
    let changed: Vec<usize> = all.iter().enumerate().filter(|(_, (c, _))| *c != ' ').map(|(k, _)| k).collect();
    let keep = |k: usize| changed.iter().any(|&c| k + context >= c && k <= c + context);
    let mut out = Vec::new();
    let mut skipped = false;
    for (k, line) in all.into_iter().enumerate() {
        if keep(k) {
            out.push(line);
            skipped = false;
        } else if !skipped {
            out.push(('…', String::new()));
            skipped = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe() -> PathBuf {
        PathBuf::from(r"C:\Program Files\tether\tether.exe")
    }

    fn user_settings() -> Value {
        json!({
            "model": "opus",
            "permissions": { "allow": ["Bash(ls:*)"] },
            "hooks": {
                "Stop": [{ "hooks": [{ "type": "command", "command": "say done" }] }],
                "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "guard.sh" }] }]
            },
            "statusLine": { "type": "command", "command": "x" }
        })
    }

    #[test]
    fn claude_merge_adds_our_hooks_and_keeps_the_users() {
        let merged = merge_claude(&user_settings(), &exe()).unwrap();
        assert!(claude_installed(&merged));
        assert_eq!(merged["model"], "opus");
        assert_eq!(merged["statusLine"]["command"], "x");
        let stop = merged["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2, "the user's Stop hook stays, ours is added");
        assert_eq!(stop[0]["hooks"][0]["command"], "say done");
        assert_eq!(stop[1]["hooks"][0]["command"], "\"C:/Program Files/tether/tether.exe\" hook claude Stop");
        assert_eq!(merged["hooks"]["PreToolUse"], user_settings()["hooks"]["PreToolUse"]);
        // Key order is kept (serde_json preserve_order): the user's keys first.
        let keys: Vec<&String> = merged.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["model", "permissions", "hooks", "statusLine"]);
    }

    #[test]
    fn claude_merge_is_idempotent_and_updates_a_moved_exe() {
        let once = merge_claude(&user_settings(), &exe()).unwrap();
        assert_eq!(merge_claude(&once, &exe()).unwrap(), once);
        let moved = merge_claude(&once, Path::new("D:/apps/tether.exe")).unwrap();
        let stop = moved["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[1]["hooks"][0]["command"], "\"D:/apps/tether.exe\" hook claude Stop");
    }

    #[test]
    fn claude_unmerge_round_trips() {
        let original = user_settings();
        let merged = merge_claude(&original, &exe()).unwrap();
        assert_eq!(unmerge_claude(&merged), original);
        assert_eq!(unmerge_claude(&original), original, "nothing of ours: unchanged");
        // A file that had no hooks at all loses the hooks object again.
        let bare = json!({ "model": "sonnet" });
        assert_eq!(unmerge_claude(&merge_claude(&bare, &exe()).unwrap()), bare);
        // An empty file becomes just our hooks.
        assert!(claude_installed(&merge_claude(&Value::Null, &exe()).unwrap()));
        assert!(merge_claude(&json!([1]), &exe()).is_err());
        assert!(merge_claude(&json!({ "hooks": { "Stop": 3 } }), &exe()).is_err());
    }

    #[test]
    fn recognises_only_our_commands() {
        assert!(is_our_claude_command("\"C:/x/tether.exe\" hook claude Stop"));
        assert!(is_our_claude_command("\"/usr/bin/tether\" hook claude Notification"));
        assert!(!is_our_claude_command("tether-notify.sh"));
        assert!(!is_our_claude_command("\"C:/x/other.exe\" hook claude Stop"));
        // Merely mentioning tether doesn't make a hook ours (uninstall would delete it).
        assert!(!is_our_claude_command("\"C:/tether/backup.exe\" hook claude Stop"));
        assert!(!is_our_claude_command("\"C:/x/tether.exe\" hook claude Stop && rm -rf ~"));
        assert!(!is_our_claude_command("echo \"tether\" hook claude Stop"));
        let theirs = toml_edit::value(toml_edit::Array::from_iter(["/opt/tether-tools/notify", "hook", "codex"]));
        assert!(!codex_notify_is_ours(&theirs));
        let ours = toml_edit::value(toml_edit::Array::from_iter(["C:/apps/tether.exe", "hook", "codex"]));
        assert!(codex_notify_is_ours(&ours));
    }

    #[test]
    fn unsafe_exe_paths_are_refused() {
        for bad in [r#"C:/a"b/tether.exe"#, "/home/$USER/tether", "/opt/`x`/tether", "C:/%TEMP%/tether.exe"] {
            assert!(merge_claude(&Value::Null, Path::new(bad)).is_err(), "{bad}");
            assert!(merge_codex("", Path::new(bad)).is_err(), "{bad}");
        }
        assert!(merge_claude(&Value::Null, &exe()).is_ok());
    }

    #[test]
    fn the_first_backup_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let plan = |original: &str, after: &str| Plan {
            target: Target::Claude,
            install: true,
            path: path.clone(),
            original: Some(original.into()),
            after: after.into(),
            diff: Vec::new(),
        };
        std::fs::write(&path, "first").unwrap();
        let bak1 = apply(&plan("first", "second")).unwrap().unwrap();
        let bak2 = apply(&plan("second", "third")).unwrap().unwrap();
        assert_ne!(bak1, bak2);
        assert_eq!(std::fs::read_to_string(&bak1).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(&bak2).unwrap(), "second");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "third");
    }

    const CODEX: &str = "# my codex config\nmodel = \"gpt-5\"\n\n[mcp_servers.docs]\ncommand = \"npx\"\nargs = [\"docs\"]\n";

    #[test]
    fn codex_merge_adds_notify_at_the_top_level() {
        let CodexMerge::Changed(text) = merge_codex(CODEX, &exe()).unwrap() else { panic!() };
        assert!(codex_installed(&text));
        let doc: toml_edit::DocumentMut = text.parse().unwrap();
        assert_eq!(doc["model"].as_str(), Some("gpt-5"));
        assert_eq!(doc["mcp_servers"]["docs"]["command"].as_str(), Some("npx"));
        assert!(doc["mcp_servers"]["docs"].get("notify").is_none(), "notify must be a root key");
        assert!(text.starts_with("# my codex config\n"), "comments are kept: {text}");
        assert_eq!(merge_codex(&text, &exe()).unwrap(), CodexMerge::AlreadyInstalled);
        assert_eq!(unmerge_codex(&text).unwrap().as_deref(), Some(CODEX));
        assert_eq!(unmerge_codex(CODEX).unwrap(), None);
    }

    #[test]
    fn codex_merge_never_overwrites_the_users_notify() {
        let theirs = "notify = [\"python3\", \"/home/me/notify.py\"]\n";
        assert!(matches!(merge_codex(theirs, &exe()).unwrap(), CodexMerge::Conflict(s) if s.contains("notify.py")));
        assert_eq!(unmerge_codex(theirs).unwrap(), None);
        assert!(merge_codex("not = [valid", &exe()).is_err());
        // Ours, from an older install path: updated in place.
        let old = "notify = [\"D:/old/tether.exe\", \"hook\", \"codex\"]\n";
        assert!(matches!(merge_codex(old, &exe()).unwrap(), CodexMerge::Changed(t) if t.contains("C:/Program Files/tether/tether.exe")));
    }

    #[test]
    fn diff_shows_changes_with_context() {
        let before = "a\nb\nc\nd\ne\nf\ng\n";
        let after = "a\nb\nc\nX\ne\nf\ng\n";
        let d = diff_lines(before, after, 1);
        assert_eq!(d, vec![('…', String::new()), (' ', "c".into()), ('+', "X".into()), ('-', "d".into()), (' ', "e".into()), ('…', String::new())]);
        assert!(diff_lines("same\n", "same\n", 2).iter().all(|(c, _)| *c == '…'));
    }

    #[test]
    fn apply_backs_up_and_refuses_stale_plans() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{\"model\":\"opus\"}").unwrap();
        let after = pretty(&merge_claude(&json!({"model":"opus"}), &exe()).unwrap());
        let plan = Plan {
            target: Target::Claude,
            install: true,
            path: path.clone(),
            original: Some("{\"model\":\"opus\"}".into()),
            after: after.clone(),
            diff: Vec::new(),
        };
        let bak = apply(&plan).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(bak).unwrap(), "{\"model\":\"opus\"}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after);
        assert!(apply(&plan).is_err(), "the file changed since the plan");
    }
}
