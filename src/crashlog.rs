//! Crash logger: installs a panic hook that writes a full report (version, timestamp,
//! thread, file:line:col, panic message, and forced stack backtrace) to
//! `<config_dir>/crash.log` and `<config_dir>/last_crash.txt`.
//!
//! Because release builds on Windows run with `windows_subsystem = "windows"` (no stderr
//! console), an unhandled panic would otherwise vanish silently without leaving a trace.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum size of `crash.log` before older entries are rotated to `crash.prev.log`.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

pub fn log_path() -> PathBuf {
    crate::app::config_dir().join("crash.log")
}

pub fn last_crash_path() -> PathBuf {
    crate::app::config_dir().join("last_crash.txt")
}

/// Installs the global panic hook. Call once at the start of `main()`.
pub fn install() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let report = format_panic_report(info, &backtrace.to_string());
        let dir = crate::app::config_dir();
        let _ = write_report_to_dir(&dir, &report);
        prev(info);
    }));
}

/// Records a fatal non-panic error (such as `eframe::run_native` failing) to `crash.log`.
pub fn record_fatal(context: &str, err: &dyn std::fmt::Display) {
    let ts = unix_timestamp();
    let version = env!("CARGO_PKG_VERSION");
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let backtrace = std::backtrace::Backtrace::force_capture();
    let report = format!(
        "=== TETHER FATAL ERROR ===\n\
         timestamp_unix: {ts}\n\
         version: {version} ({os}/{arch})\n\
         context: {context}\n\
         error: {err}\n\
         backtrace:\n{backtrace}\n\
         ==========================\n\n"
    );
    let _ = write_report_to_dir(&crate::app::config_dir(), &report);
}

/// If the previous run crashed and left `last_crash.txt`, removes that marker and returns
/// a short summary line `(location_and_message, crash_log_path)` so the UI can surface it.
pub fn take_last_crash_summary() -> Option<(String, PathBuf)> {
    let marker = last_crash_path();
    let content = fs::read_to_string(&marker).ok()?;
    let _ = fs::remove_file(&marker);
    let summary = extract_summary(&content);
    Some((summary, log_path()))
}

fn format_panic_report(info: &PanicHookInfo<'_>, backtrace: &str) -> String {
    let ts = unix_timestamp();
    let version = env!("CARGO_PKG_VERSION");
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let thread = std::thread::current();
    let thread_name = thread.name().unwrap_or("<unnamed>");
    let location = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_else(|| "<unknown location>".to_string());
    let message = panic_payload_message(info);

    format!(
        "=== TETHER CRASH REPORT ===\n\
         timestamp_unix: {ts}\n\
         version: {version} ({os}/{arch})\n\
         thread: {thread_name}\n\
         location: {location}\n\
         message: {message}\n\
         backtrace:\n{backtrace}\n\
         ===========================\n\n"
    )
}

fn panic_payload_message(info: &PanicHookInfo<'_>) -> String {
    if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "Box<dyn Any>".to_string()
    }
}

fn write_report_to_dir(dir: &Path, report: &str) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let log = dir.join("crash.log");
    if let Ok(meta) = fs::metadata(&log)
        && meta.len() >= MAX_LOG_BYTES
    {
        let prev = dir.join("crash.prev.log");
        let _ = fs::rename(&log, prev);
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&log)?;
    file.write_all(report.as_bytes())?;
    file.flush()?;

    let last = dir.join("last_crash.txt");
    fs::write(last, report.as_bytes())?;
    Ok(())
}

fn extract_summary(report: &str) -> String {
    let mut location = None;
    let mut message = None;
    for line in report.lines() {
        if let Some(loc) = line.strip_prefix("location: ") {
            location = Some(loc.trim());
        } else if let Some(msg) = line.strip_prefix("message: ") {
            message = Some(msg.trim());
        }
    }
    match (location, message) {
        (Some(loc), Some(msg)) => format!("{msg} ({loc})"),
        (None, Some(msg)) => msg.to_string(),
        (Some(loc), None) => loc.to_string(),
        (None, None) => "Unexpected panic".to_string(),
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_rotates_crash_reports() {
        let dir = tempfile::tempdir().unwrap();
        let sample = "=== TETHER CRASH REPORT ===\n\
                      timestamp_unix: 1700000000\n\
                      version: 0.2.2\n\
                      thread: main\n\
                      location: src/ui/term_links.rs:520:18\n\
                      message: byte index 1 is not a char boundary\n\
                      ===========================\n\n";
        write_report_to_dir(dir.path(), sample).unwrap();
        let written = fs::read_to_string(dir.path().join("crash.log")).unwrap();
        assert!(written.contains("src/ui/term_links.rs:520:18"));
        let summary = extract_summary(&written);
        assert_eq!(summary, "byte index 1 is not a char boundary (src/ui/term_links.rs:520:18)");
    }
}
