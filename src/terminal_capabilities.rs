//! Capabilities shared by local PTYs and remote SSH terminals. UI processes may be
//! launched by a non-interactive runner; its color policy is not the terminal's.

pub const TERM: &str = "xterm-256color";
pub const ENV: &[(&str, &str)] =
    &[("TERM", TERM), ("COLORTERM", "truecolor"), ("TERM_PROGRAM", "tether"), ("FORCE_COLOR", "3"), ("CLICOLOR", "1"), ("CLICOLOR_FORCE", "1")];

pub fn local(cmd: &mut portable_pty::CommandBuilder) {
    cmd.env_remove("NO_COLOR");
    for &(key, value) in ENV {
        cmd.env(key, value);
    }
}
