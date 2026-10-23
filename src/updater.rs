//! Self-update from GitHub Releases.
//!
//! On startup (Settings → About → "Check for updates on startup") a background thread
//! asks the GitHub API for the latest release. When it is newer than this build, the
//! top bar offers the platform installer: the `Tether-Setup-<ver>.exe` asset is downloaded to a temp
//! folder, checked against its published `.sha256`, started with `/SILENT`, and tether
//! quits on Windows. On macOS a verified architecture-specific DMG opens in Finder;
//! the app stays running while the user chooses when to replace it in Applications.
//!
//! The state is a process-wide singleton so the top bar and the About page share it.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use eframe::egui;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const REPO: &str = "izLoki/Tether";
const LATEST_URL: &str = "https://api.github.com/repos/izLoki/Tether/releases/latest";
const TIMEOUT: Duration = Duration::from_secs(10);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// Refuse absurd assets (the installer is ~10 MB).
const MAX_INSTALLER_BYTES: u64 = 200 * 1024 * 1024;
/// Asset names are shared with scripts/package-macos.sh and the release workflow.
fn installer_name(version: &Version, os: &str, arch: &str) -> String {
    match os {
        "macos" => format!("Tether-{version}-macos-{arch}.dmg"),
        _ => format!("Tether-Setup-{version}.exe"),
    }
}

pub const fn install_label() -> &'static str {
    if cfg!(target_os = "macos") { "Open installer" } else { "Install" }
}

pub const fn install_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "Downloads and verifies the disk image. Drag Tether to Applications, then restart Tether."
    } else {
        "Downloads the installer from GitHub, installs it and restarts tether"
    }
}

/// A release newer than this build, ready to install.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: Version,
    pub page: String,
    installer: Asset,
    checksum: Asset,
}

#[derive(Debug, Clone, PartialEq)]
struct Asset {
    name: String,
    url: String,
    size: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading {
        version: Version,
        got: u64,
        total: u64,
    },
    /// The installer is running; the app is closing.
    Installing,
    /// macOS mounted the verified disk image; the user replaces the app in Finder.
    InstallerOpened(Release),
    /// The check failed (offline, rate limited…): only shown on the About page.
    Failed(String),
    /// Downloading or starting the installer failed: shown in the top bar with a retry.
    InstallFailed(Release, String),
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::Idle));

pub fn state() -> State {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn set(ctx: &egui::Context, s: State) {
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = s;
    ctx.request_repaint();
}

/// Called once at startup.
pub fn check_on_startup(ctx: &egui::Context) {
    if crate::settings::get().updates.check_on_startup && !cfg!(debug_assertions) {
        check(ctx);
    }
}

/// Starts a background check unless one is already running.
pub fn check(ctx: &egui::Context) {
    if matches!(state(), State::Checking | State::Downloading { .. } | State::Installing) {
        return;
    }
    set(ctx, State::Checking);
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name("tether-update-check".into())
        .spawn(move || {
            let next = match fetch_latest() {
                Ok(Some(release)) => State::Available(release),
                Ok(None) => State::UpToDate,
                Err(e) => State::Failed(format!("{e:#}")),
            };
            set(&ctx, next);
        })
        .ok();
}

/// Downloads and launches the installer; Windows exits, macOS stays open for Finder.
pub fn install(ctx: &egui::Context) {
    let (State::Available(release) | State::InstallFailed(release, _) | State::InstallerOpened(release)) = state() else { return };
    set(ctx, State::Downloading { version: release.version.clone(), got: 0, total: release.installer.size });
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name("tether-update-install".into())
        .spawn(move || match download_and_launch(&ctx, &release) {
            Ok(()) => {
                if cfg!(target_os = "macos") {
                    set(&ctx, State::InstallerOpened(release));
                } else {
                    set(&ctx, State::Installing);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Err(e) => set(&ctx, State::InstallFailed(release, format!("{e:#}"))),
        })
        .ok();
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(TIMEOUT).timeout(DOWNLOAD_TIMEOUT).https_only(true).build()
}

fn get(url: &str) -> Result<ureq::Response> {
    agent()
        .get(url)
        .set("User-Agent", concat!("tether/", env!("CARGO_PKG_VERSION")))
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| anyhow!("{e}"))
}

fn fetch_latest() -> Result<Option<Release>> {
    let body = get(LATEST_URL)?.into_string().context("reading the release info")?;
    let current = Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is semver");
    newer_release(&body, &current)
}

// ---- parsing (pure, unit-tested) ------------------------------------------------

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// The release in `json` if it is newer than `current` and ships a valid installer.
fn newer_release(json: &str, current: &Version) -> Result<Option<Release>> {
    newer_release_for(json, current, std::env::consts::OS, std::env::consts::ARCH)
}

fn newer_release_for(json: &str, current: &Version, os: &str, arch: &str) -> Result<Option<Release>> {
    let api: ApiRelease = serde_json::from_str(json).context("unexpected answer from GitHub")?;
    if api.draft || api.prerelease {
        return Ok(None);
    }
    let version = Version::parse(api.tag_name.trim_start_matches('v')).with_context(|| format!("release tag “{}” is not a version", api.tag_name))?;
    if version <= *current {
        return Ok(None);
    }
    let installer_name = installer_name(&version, os, arch);
    let installer = pick_asset(&api.assets, &installer_name).ok_or_else(|| anyhow!("release v{version} has no {installer_name}"))?;
    ensure!(installer.size > 0 && installer.size <= MAX_INSTALLER_BYTES, "the installer of v{version} has an unexpected size");
    let checksum = pick_asset(&api.assets, &format!("{installer_name}.sha256")).ok_or_else(|| anyhow!("release v{version} has no checksum"))?;
    let page = if is_trusted_url(&api.html_url) { api.html_url } else { format!("https://github.com/{REPO}/releases") };
    Ok(Some(Release { version, page, installer, checksum }))
}

fn pick_asset(assets: &[ApiAsset], name: &str) -> Option<Asset> {
    assets.iter().find(|a| a.name == name && is_trusted_url(&a.browser_download_url)).map(|a| Asset {
        name: a.name.clone(),
        url: a.browser_download_url.clone(),
        size: a.size,
    })
}

/// Only downloads from this repository's GitHub pages (redirects to GitHub's CDN are
/// followed by ureq, which never downgrades to plain http).
fn is_trusted_url(url: &str) -> bool {
    let prefix = format!("https://github.com/{REPO}/");
    url.starts_with(&prefix) && !url.contains("..") && !url.contains('@')
}

/// The SHA-256 in a `sha256sum`-style file (`<hex>  <name>` or just `<hex>`).
fn parse_checksum(text: &str) -> Option<[u8; 32]> {
    let hex = text.split_whitespace().next()?;
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

// ---- download + launch ----------------------------------------------------------

fn download_and_launch(ctx: &egui::Context, release: &Release) -> Result<()> {
    let sums = get(&release.checksum.url)?.into_string().context("downloading the checksum")?;
    let expected = parse_checksum(&sums).ok_or_else(|| anyhow!("the checksum file is malformed"))?;
    let dir = std::env::temp_dir().join("tether-update");
    std::fs::create_dir_all(&dir).context("creating the download folder")?;
    let path = dir.join(&release.installer.name);
    download(ctx, release, &path, &expected)?;
    launch(&path)
}

fn download(ctx: &egui::Context, release: &Release, path: &Path, expected: &[u8; 32]) -> Result<()> {
    let total = release.installer.size;
    let mut reader = get(&release.installer.url)?.into_reader().take(MAX_INSTALLER_BYTES + 1);
    let partial = path.with_extension("part");
    let mut file = std::fs::File::create(&partial).with_context(|| format!("creating {}", partial.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut got = 0u64;
    loop {
        let n = reader.read(&mut buf).context("downloading the installer")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).context("writing the installer")?;
        got += n as u64;
        set(ctx, State::Downloading { version: release.version.clone(), got, total });
    }
    file.sync_all().ok();
    drop(file);
    let result = (|| {
        ensure!(got == total, "download incomplete ({got} of {total} bytes)");
        ensure!(hasher.finalize().as_slice() == expected, "the installer's checksum doesn't match; not running it");
        std::fs::rename(&partial, path).context("saving the installer")
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}

fn launch(installer: &Path) -> Result<()> {
    if cfg!(target_os = "macos") {
        let status = std::process::Command::new("/usr/bin/open").arg(installer).status().context("opening the macOS disk image")?;
        ensure!(status.success(), "macOS could not open the disk image ({status})");
        return Ok(());
    }
    if !cfg!(windows) {
        bail!("automatic install is unavailable on this platform; download it from the release page");
    }
    std::process::Command::new(installer)
        .args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/RELAUNCH=1"])
        .spawn()
        .with_context(|| format!("starting {}", installer.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Windows fixtures still verify the original installer on every test host.
    fn newer_release(json: &str, current: &Version) -> Result<Option<Release>> {
        super::newer_release_for(json, current, "windows", "x86_64")
    }

    #[test]
    fn macos_updates_select_matching_architecture_and_require_checksums() {
        for arch in ["aarch64", "x86_64"] {
            let name = installer_name(&Version::new(0, 2, 0), "macos", arch);
            let base = "https://github.com/izLoki/Tether/releases/download/v0.2.0";
            let assets =
                [good_assets("0.2.0"), asset(&name, &format!("{base}/{name}")), asset(&format!("{name}.sha256"), &format!("{base}/{name}.sha256"))].join(",");
            let release = super::newer_release_for(&release_json("v0.2.0", &assets), &current(), "macos", arch).unwrap().unwrap();
            assert_eq!(release.installer.name, name);
            assert!(super::newer_release_for(&release_json("v0.2.0", &good_assets("0.2.0")), &current(), "macos", arch).is_err());
            let missing_sum = asset(&name, &format!("{base}/{name}"));
            assert!(super::newer_release_for(&release_json("v0.2.0", &missing_sum), &current(), "macos", arch).is_err());
            let foreign = assets.replace("https://github.com/", "https://evil.example/");
            assert!(super::newer_release_for(&release_json("v0.2.0", &foreign), &current(), "macos", arch).is_err());
        }
    }

    fn release_json(tag: &str, assets: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.com/izLoki/Tether/releases/tag/{tag}","draft":false,"prerelease":false,"assets":[{assets}]}}"#
        )
    }

    fn asset(name: &str, url: &str) -> String {
        format!(r#"{{"name":"{name}","browser_download_url":"{url}","size":1234}}"#)
    }

    fn good_assets(v: &str) -> String {
        let base = format!("https://github.com/izLoki/Tether/releases/download/v{v}");
        [
            asset("tether.exe", &format!("{base}/tether.exe")),
            asset(&format!("Tether-Setup-{v}.exe"), &format!("{base}/Tether-Setup-{v}.exe")),
            asset(&format!("Tether-Setup-{v}.exe.sha256"), &format!("{base}/Tether-Setup-{v}.exe.sha256")),
        ]
        .join(",")
    }

    fn current() -> Version {
        Version::new(0, 1, 0)
    }

    #[test]
    fn newer_release_with_installer_is_offered() {
        let r = newer_release(&release_json("v0.2.0", &good_assets("0.2.0")), &current()).unwrap().unwrap();
        assert_eq!(r.version, Version::new(0, 2, 0));
        assert_eq!(r.installer.name, "Tether-Setup-0.2.0.exe");
        assert!(r.checksum.url.ends_with(".sha256"));
    }

    #[test]
    fn same_or_older_versions_are_ignored() {
        assert_eq!(newer_release(&release_json("v0.1.0", &good_assets("0.1.0")), &current()).unwrap(), None);
        assert_eq!(newer_release(&release_json("0.0.9", &good_assets("0.0.9")), &current()).unwrap(), None);
        // Semver, not string order: 0.10 > 0.9.
        assert!(newer_release(&release_json("v0.10.0", &good_assets("0.10.0")), &Version::new(0, 9, 0)).unwrap().is_some());
    }

    #[test]
    fn prereleases_and_drafts_are_ignored() {
        let pre = release_json("v0.2.0", &good_assets("0.2.0")).replace(r#""prerelease":false"#, r#""prerelease":true"#);
        assert_eq!(newer_release(&pre, &current()).unwrap(), None);
        let draft = release_json("v0.2.0", &good_assets("0.2.0")).replace(r#""draft":false"#, r#""draft":true"#);
        assert_eq!(newer_release(&draft, &current()).unwrap(), None);
    }

    #[test]
    fn foreign_hosts_and_wrong_names_are_rejected() {
        let evil = [
            asset("Tether-Setup-0.2.0.exe", "https://evil.example/Tether-Setup-0.2.0.exe"),
            asset("Tether-Setup-0.2.0.exe.sha256", "https://github.com/izLoki/Tether/releases/download/v0.2.0/x.sha256"),
        ]
        .join(",");
        assert!(newer_release(&release_json("v0.2.0", &evil), &current()).is_err());
        let other_repo = good_assets("0.2.0").replace("izLoki/Tether", "someone/Tether");
        assert!(newer_release(&release_json("v0.2.0", &other_repo), &current()).is_err());
        let http = good_assets("0.2.0").replace("https://", "http://");
        assert!(newer_release(&release_json("v0.2.0", &http), &current()).is_err());
        let wrong_version = good_assets("0.1.5");
        assert!(newer_release(&release_json("v0.2.0", &wrong_version), &current()).is_err());
    }

    #[test]
    fn missing_checksum_is_rejected() {
        let base = "https://github.com/izLoki/Tether/releases/download/v0.2.0";
        let only_setup = asset("Tether-Setup-0.2.0.exe", &format!("{base}/Tether-Setup-0.2.0.exe"));
        assert!(newer_release(&release_json("v0.2.0", &only_setup), &current()).is_err());
    }

    #[test]
    fn bad_tags_are_errors() {
        assert!(newer_release(&release_json("latest", "[]"), &current()).is_err());
        assert!(newer_release("not json", &current()).is_err());
    }

    #[test]
    fn checksum_files_parse() {
        let hex = "ab".repeat(32);
        assert_eq!(parse_checksum(&format!("{hex}  Tether-Setup-0.2.0.exe\n")), Some([0xab; 32]));
        assert_eq!(parse_checksum(&hex.to_uppercase()), Some([0xab; 32]));
        assert_eq!(parse_checksum("abc"), None);
        assert_eq!(parse_checksum(&"zz".repeat(32)), None);
        assert_eq!(parse_checksum(""), None);
    }
}
