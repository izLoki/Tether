//! Desktop conventions shared by menus, shortcuts and platform integrations.

pub const fn primary_label() -> &'static str {
    if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" }
}

pub fn shortcut(key: &str) -> String {
    format!("{}+{key}", primary_label())
}

pub const fn credential_store() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS Keychain"
    } else if cfg!(windows) {
        "Windows Credential Manager"
    } else {
        "system credential store"
    }
}

pub const fn open_folder_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "📁  Open folder in Finder"
    } else if cfg!(windows) {
        "📁  Open folder in Explorer"
    } else {
        "📁  Open folder"
    }
}

pub const fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "📂  Show in Finder"
    } else if cfg!(windows) {
        "📂  Show in Explorer"
    } else {
        "📂  Show in file manager"
    }
}

/// Finder and Dock launches need not inherit the user's terminal PATH. Resolve it
/// without mutating the process environment (other threads may already be running).
/// Only shell/agent detection calls this, on background threads.
pub fn executable_dirs() -> &'static [std::path::PathBuf] {
    static DIRS: std::sync::OnceLock<Vec<std::path::PathBuf>> = std::sync::OnceLock::new();
    DIRS.get_or_init(|| {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs: Vec<_> = std::env::split_paths(&path).collect();
        #[cfg(target_os = "macos")]
        {
            let mut command = std::process::Command::new(login_shell());
            command.args(["-l", "-i", "-c", "printf '\\0%s\\0' \"$PATH\""]);
            if let Some(output) = crate::pty::shells::stdout_within(command, std::time::Duration::from_secs(2)) {
                // Ignore output from login scripts before/after the NUL-delimited value.
                if let Some(value) = output.split(|b| *b == 0).nth(1) {
                    use std::os::unix::ffi::OsStrExt;
                    dirs.extend(std::env::split_paths(std::ffi::OsStr::from_bytes(value)));
                }
            }
            dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(std::path::PathBuf::from));
            if let Some(home) = directories::BaseDirs::new() {
                dirs.push(home.home_dir().join(".local/bin"));
                dirs.push(home.home_dir().join(".cargo/bin"));
            }
        }
        let mut unique = Vec::new();
        for dir in dirs.drain(..).filter(|d| d.is_absolute()) {
            if !unique.contains(&dir) {
                unique.push(dir);
            }
        }
        unique
    })
}

#[cfg(target_os = "macos")]
pub fn login_shell() -> std::path::PathBuf {
    use std::ffi::CStr;
    // getpwuid_r uses caller-owned memory, unlike getpwuid's shared static buffer.
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut buffer = vec![0u8; 16 * 1024];
    let mut result = std::ptr::null_mut();
    // SAFETY: all pointers refer to live, correctly sized storage until after the copy.
    let status = unsafe { libc::getpwuid_r(libc::getuid(), entry.as_mut_ptr(), buffer.as_mut_ptr().cast(), buffer.len(), &mut result) };
    if status == 0 && !result.is_null() {
        // SAFETY: successful getpwuid_r initialized entry and pw_shell is in buffer.
        let shell = unsafe { (*result).pw_shell };
        if !shell.is_null() {
            use std::os::unix::ffi::OsStrExt;
            let path = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(unsafe { CStr::from_ptr(shell) }.to_bytes()));
            if path.is_absolute() && path.is_file() {
                return path;
            }
        }
    }
    std::env::var_os("SHELL").map(std::path::PathBuf::from).filter(|p| p.is_absolute() && p.is_file()).unwrap_or_else(|| "/bin/zsh".into())
}
