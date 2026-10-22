//! Finds the working directory of one of our interactive shells, so an SFTP pane can
//! jump to "where the terminal next to it is".
//!
//! Over a separate exec channel a small POSIX script lists every login shell of the
//! user (a process with a tty whose parent is sshd / dropbear) with: whether it carries
//! our `LC_TETHER_ID=<hex tag>` (sent with the pty request; many servers drop it, e.g.
//! RHEL only accepts named `LC_*` variables and Alpine none at all), its start time, its
//! tty size and the cwd of the tty's foreground process (so `sudo -i`, a nested shell or
//! `vim` count, not just the login shell). The tagged shell wins; otherwise the shell
//! whose size and age match this terminal. Linux (`/proc`) only.

use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::connection::{Connection, exec};

const EXEC_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_OUTPUT: usize = 256 * 1024;
/// A shell started this many seconds away from our terminal is not ours.
const MAX_AGE_SKEW: f64 = 20.0;
const MAX_PATH: usize = 4096;

/// What we know about our terminal on the client side.
#[derive(Debug, Clone, Copy)]
pub struct Probe<'a> {
    pub tag: &'a str,
    /// Seconds since the shell channel was opened.
    pub age_secs: f64,
    pub rows: u16,
    pub cols: u16,
}

/// The lookup script (no single quotes or backslash pairs, so it survives `sh -c '…'`
/// under any login shell, fish included).
pub fn lookup_command(tag: &str) -> Result<String> {
    if tag.is_empty() || !tag.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid terminal tag");
    }
    let script = format!(
        r#"clk=$(getconf CLK_TCK 2>/dev/null); [ -n "$clk" ] || clk=100
read -r up idle 2>/dev/null </proc/uptime || exit 4
echo "U $up $clk"
for d in /proc/[0-9]*; do
  [ -r "$d/stat" ] || continue
  read -r st 2>/dev/null <"$d/stat" || continue
  set -- ${{st##*") "}}
  [ "$5" != 0 ] || continue
  pc=; read -r pc 2>/dev/null <"/proc/$2/comm"
  case "$pc" in sshd*|dropbear*) ;; *) continue ;; esac
  fg=$6; start=${{20}}; t=0
  grep -qsa "LC_TETHER_ID={tag}" "$d/environ" && t=1
  tty=$(readlink "$d/fd/0" 2>/dev/null)
  sz=$(stty -F "$tty" size 2>/dev/null); [ -n "$sz" ] || sz="0 0"
  cwd=$(readlink "/proc/$fg/cwd" 2>/dev/null); [ -n "$cwd" ] || cwd=$(readlink "$d/cwd" 2>/dev/null)
  [ -n "$cwd" ] && printf "P %s %s %s %s\n" "$t" "$start" "$sz" "$cwd"
done
exit 0"#
    );
    debug_assert!(!script.contains('\''));
    Ok(format!("sh -c '{script}'"))
}

#[derive(Debug, Clone, PartialEq)]
struct Candidate {
    tagged: bool,
    age: f64,
    size: (u16, u16),
    cwd: String,
}

/// Parses the script output and picks the shell of the probed terminal.
fn pick(output: &str, probe: &Probe) -> Option<String> {
    let mut lines = output.lines();
    let header: Vec<&str> = lines.next()?.split_whitespace().collect();
    let (uptime, clk) = match header.as_slice() {
        ["U", up, clk] => (up.parse::<f64>().ok()?, clk.parse::<f64>().ok().filter(|c| *c > 0.0)?),
        _ => return None,
    };
    let candidates: Vec<Candidate> = lines
        .filter_map(|line| {
            let mut f = line.splitn(6, ' ');
            if f.next()? != "P" {
                return None;
            }
            let tagged = f.next()? == "1";
            let start: f64 = f.next()?.parse().ok()?;
            let size = (f.next()?.parse().ok()?, f.next()?.parse().ok()?);
            let cwd = f.next()?.to_string();
            // The path comes from the server: plain absolute paths only.
            let sane = cwd.starts_with('/') && cwd.len() <= MAX_PATH && !cwd.chars().any(char::is_control);
            sane.then_some(Candidate { tagged, age: uptime - start / clk, size, cwd })
        })
        .collect();
    if let Some(c) = candidates.iter().find(|c| c.tagged) {
        return Some(c.cwd.clone());
    }
    let same_size: Vec<&Candidate> = candidates.iter().filter(|c| c.size == (probe.rows, probe.cols)).collect();
    let pool: Vec<&Candidate> = if same_size.is_empty() { candidates.iter().collect() } else { same_size };
    let skew = |c: &Candidate| (c.age - probe.age_secs).abs();
    let best = pool.iter().min_by(|a, b| skew(a).total_cmp(&skew(b)))?;
    (candidates.len() == 1 || skew(best) <= MAX_AGE_SKEW).then(|| best.cwd.clone())
}

pub async fn terminal_cwd(conn: &Connection, probe: Probe<'_>) -> Result<String> {
    let command = lookup_command(probe.tag)?;
    let (out, status) = tokio::time::timeout(EXEC_TIMEOUT, exec(conn, command, MAX_OUTPUT)).await.context("timed out asking the server")??;
    let text = String::from_utf8_lossy(&out);
    if status == Some(4) || !text.starts_with('U') {
        bail!("Couldn't read the terminal's folder: the server has no /proc (Linux only)");
    }
    pick(&text, &probe).ok_or_else(|| anyhow::anyhow!("Couldn't tell which shell belongs to this terminal. Tip: let the shell report its folder (OSC 7)"))
}

/// Usernames accepted in a prompt's named-home reference. No shell syntax is allowed.
pub fn valid_home_user(user: &str) -> bool {
    !user.is_empty() && user.len() <= 256 && !user.starts_with('-') && user.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
}

pub fn user_home_command(user: &str) -> Result<String> {
    if !valid_home_user(user) {
        bail!("Invalid username in remote home path");
    }
    // NSS-aware on Linux; /etc/passwd also covers systems without getent. The
    // validated username cannot escape the quotes or introduce shell expansion.
    Ok(format!("sh -c 'getent passwd \"{user}\" 2>/dev/null || grep -F \"{user}:\" /etc/passwd'"))
}

fn parse_user_home(output: &str, user: &str) -> Result<String> {
    for entry in output.lines() {
        let mut fields = entry.split(':');
        if fields.next() != Some(user) {
            continue;
        }
        if let Some(home) = fields.nth(4)
            && home.starts_with('/')
            && home.len() <= MAX_PATH
            && !home.chars().any(char::is_control)
        {
            return Ok(home.into());
        }
    }
    bail!("Could not resolve the remote home directory for {user}")
}

pub async fn user_home(conn: &Connection, user: &str) -> Result<String> {
    let command = user_home_command(user)?;
    let (out, _) = tokio::time::timeout(EXEC_TIMEOUT, exec(conn, command, MAX_OUTPUT)).await.context("timed out resolving remote user's home")??;
    parse_user_home(std::str::from_utf8(&out).context("invalid remote home reply")?, user)
}

/// Resolve remote paths for both SFTP navigation and the editor. In particular,
/// ~prisma uses prisma's actual home, not the SSH transport's authenticated home.
pub async fn resolve_path(conn: &Connection, sftp: &super::sftp::Sftp, path: &str, cwd: &str) -> Result<String> {
    if path.starts_with('/') {
        return Ok(path.into());
    }
    if let Some(rest) = path.strip_prefix('~') {
        let (user, tail) = rest.split_once('/').unwrap_or((rest, ""));
        let home = if user.is_empty() { sftp.canonicalize(".").await? } else { user_home(conn, user).await? };
        return Ok(if tail.is_empty() { home } else { super::sftp::join(&home, tail) });
    }
    Ok(super::sftp::join(cwd, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_home_uses_the_prompt_account_and_its_actual_server_home() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\nprisma:x:1001:1001::/srv/custom home/prisma:/bin/bash\n";
        assert_eq!(parse_user_home(passwd, "prisma").unwrap(), "/srv/custom home/prisma");
        assert_eq!(parse_user_home(passwd, "root").unwrap(), "/root");
        assert!(parse_user_home(passwd, "missing").is_err());
        assert!(parse_user_home("prisma:x:1:1::relative:/bin/sh", "prisma").is_err());
        assert!(user_home_command("prisma").unwrap().contains("getent passwd \"prisma\""));
        for bad in ["", "-root", "prisma;id", "$(id)", "a\"b", "a'b", "a/b", "a\nb"] {
            assert!(user_home_command(bad).is_err(), "{bad:?}");
        }
    }

    const PROBE: Probe = Probe { tag: "ab12", age_secs: 30.0, rows: 40, cols: 120 };

    #[test]
    fn osc7_paths_decode() {
        use crate::ssh::terminal::osc7_path;
        assert_eq!(osc7_path(b"file://box/home/ana/my%20dir").as_deref(), Some("/home/ana/my dir"));
        assert_eq!(osc7_path(b"file:///srv").as_deref(), Some("/srv"));
        assert_eq!(osc7_path(b"http://x/y"), None);
        assert_eq!(osc7_path(b"file://h/100%").as_deref(), Some("/100%"));
    }

    #[test]
    fn command_embeds_only_hex_tags() {
        let cmd = lookup_command("abc123").unwrap();
        assert!(cmd.contains("LC_TETHER_ID=abc123"));
        assert!(cmd.starts_with("sh -c '") && cmd.ends_with('\''));
        assert_eq!(cmd.matches('\'').count(), 2, "the script itself has no single quotes");
        assert!(lookup_command("x'; rm -rf / #").is_err());
        assert!(lookup_command("").is_err());
    }

    #[test]
    fn tagged_shell_wins() {
        // uptime 1000s, CLK_TCK 100: start 97000 ticks = age 30s.
        let out = "U 1000.00 100\nP 0 97000 40 120 /wrong\nP 1 10000 24 80 /home/ana/my dir\n";
        assert_eq!(pick(out, &PROBE).as_deref(), Some("/home/ana/my dir"));
    }

    #[test]
    fn untagged_matches_size_then_age() {
        let out = "U 1000 100\nP 0 97000 24 80 /same-age-wrong-size\nP 0 50000 40 120 /old\nP 0 96500 40 120 /srv/app\n";
        assert_eq!(pick(out, &PROBE).as_deref(), Some("/srv/app"));
    }

    #[test]
    fn falls_back_to_age_when_no_size_matches() {
        let out = "U 1000 100\nP 0 10000 24 80 /old\nP 0 97100 30 90 /near\n";
        assert_eq!(pick(out, &PROBE).as_deref(), Some("/near"));
    }

    #[test]
    fn refuses_to_guess_between_far_candidates() {
        let out = "U 1000 100\nP 0 10000 40 120 /a\nP 0 20000 40 120 /b\n";
        assert_eq!(pick(out, &PROBE), None);
        let single = "U 1000 100\nP 0 10000 40 120 /only\n";
        assert_eq!(pick(single, &PROBE).as_deref(), Some("/only"), "a lone shell must be ours");
    }

    #[test]
    fn server_paths_with_control_characters_are_rejected() {
        let out = "U 1000 100\nP 1 97000 40 120 /evil\u{1b}[2J\n";
        assert_eq!(pick(out, &PROBE), None);
    }

    #[test]
    fn garbage_output_is_rejected() {
        assert_eq!(pick("", &PROBE), None);
        assert_eq!(pick("hello\nP 1 1 1 1 /x", &PROBE), None);
        assert_eq!(pick("U 1000 100\nP 1 1 1 1 relative", &PROBE), None);
    }
}
