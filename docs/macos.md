# macOS support and QA

## Install and build

Use the release DMG matching the Mac: `aarch64` for Apple Silicon or `x86_64` for
Intel. Open it, drag **Tether.app** to **Applications**, and launch Tether from Finder
or the Dock. The updater verifies the published SHA-256 before opening the matching
DMG. Quit Tether, replace it in Applications, then reopen to finish updating.

Build locally with the stable Rust toolchain and Xcode Command Line Tools:

```sh
cargo build --release --locked
scripts/package-macos.sh                    # native architecture
scripts/package-macos.sh x86_64-apple-darwin # after rustup target add x86_64-apple-darwin
```

The script produces `dist/Tether-<version>-macos-<arch>.dmg` and its checksum. It
creates a Retina-aware `.app` with an icon, bundle identity and an Applications
shortcut in the disk image. The declared deployment floor is macOS 11; runtime QA
on that oldest OS remains required before claiming it is tested there.

Current packages use an ad-hoc signature for local execution. They are **not
Developer ID signed or notarized**. Public downloads may be blocked by Gatekeeper.
Maintainers need a Developer ID certificate, hardened-runtime signing, Apple
notarization and stapling before advertising a frictionless public install. Do not
instruct users to disable Gatekeeper or remove quarantine attributes. See
[Apple's notarization documentation](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).

## Desktop behavior

| Feature | macOS behavior |
| --- | --- |
| New host | `⌘N` (leaves `⌘ShiftN` for folders) |
| New terminal / close tab | `⌘T` / `⌘W` |
| Split right / below | `⌘D` / `⌘ShiftD` |
| Settings / command palette | `⌘,` / `⌘ShiftP` |
| Next / previous tab | `CtrlTab` / `CtrlShiftTab` |
| Terminal copy / paste | `⌘C` / `⌘V`; copy without a selection leaves the process running |
| Terminal controls | Physical `CtrlC`, `CtrlD`, `CtrlU`, `CtrlZ` still go to the shell |
| Text with Option | Native Unicode input, without a Meta escape prefix |
| Font size | `⌘+`, `⌘-`, `⌘0`; pinch zoom supported |
| File selection | Command-click toggles, Shift-click selects a range |
| Rename / open selected | Return / `⌘↓`; double-click also opens |
| Parent / back / forward | `⌘↑` / `⌘[` / `⌘]` |
| New folder / refresh | `⌘ShiftN` / `⌘R` |
| Delete files | `⌘⌫`, followed by confirmation; remote deletion is permanent |
| Finder integration | Native file clipboard URLs, file drops, Show in Finder, default system apps |
| Passwords and passphrases | macOS Keychain; private key files are owner-only |
| Local shell | Account login shell, with login configuration and bash/zsh integration |
| CLI agents | Shell PATH discovery plus Homebrew/per-user paths for Finder launches |
| Themes and fonts | Shared themes plus installed macOS monospace fonts, including TTC collections |
| Updates | Architecture-specific, checksum-verified DMG opened in Finder |

Other actions remain rebindable in Settings → Keyboard. Saved physical Control
shortcuts keep their original meaning; reset individual shortcuts or all shortcuts
to adopt native defaults when moving settings from Windows.

Configuration uses `directories::ProjectDirs`: normally
`~/Library/Application Support/dev.tether.tether/` on macOS. Set
`TETHER_CONFIG_DIR` to use an isolated folder or a portable configuration.

SSH connections, host-key verification, OpenSSH/Bitvise imports, key management,
pipelined SFTP, transfer progress/cancellation, file editing with automatic upload,
workspaces, broadcast, splits, detached windows, agent hooks, notifications and
process monitoring use the shared implementation. WSL and Windows shells are
represented by the installed native macOS shells; Windows-only executables are not
local shell candidates on a Mac.

## Automated checks

CI runs formatting, strict Clippy, library tests, real PTY tests, bash integration,
macOS zsh integration, rendered UI flows and layout tests on Windows and both Mac
architectures. macOS jobs also build/package the app. UI images and disk images
are retained as CI artifacts. Release jobs publish separate DMGs and checksums.

For network tests, follow `tests/e2e_ssh.rs`; the fixture uses an isolated OpenSSH
server bound to `127.0.0.1:2222` with disposable credentials:

```sh
TETHER_E2E=1 cargo test --locked --test e2e_ssh -- --nocapture --test-threads=1
TETHER_E2E=1 cargo test --locked --test ui_flow -- --test-threads=1
```

`bench_pipeline_sizes` requires `TETHER_BENCH=1` separately. Native Keychain tests
are opt-in because CI's login keychain can be locked. Performance benchmarks marked
ignored are not part of functional coverage.

The results and limits of the implementation QA are recorded in [macOS QA](macos-qa.md).

## Manual release matrix

Run on Apple Silicon and Intel, record macOS versions and results. Automated
headless rendering supplements these checks; it does not replace native QA.

- Install a downloaded, quarantined DMG on a clean Mac. Check signature,
  notarization, icon, Applications drag target, launch and upgrade without loss of
  profiles, keys or workspaces. Confirm the oldest declared supported OS too.
- Launch from Finder/Dock without a terminal environment. Open the account default
  shell and Homebrew bash/fish when installed. Check prompt themes, PATH, Unicode
  paths, cwd, exit status, resize, copy/paste, IME composition, Option characters and
  shell controls. Close tabs and verify background subprocess cleanup.
- Verify `⌘,`, `⌘T`, `⌘W`, splits, palette, pane navigation, font controls and rebinding.
  Check windows' traffic lights, minimize, fullscreen, hide/unhide, Cmd-Q, detached
  windows on another display, Retina scaling and workspace restore.
- Connect to a real SSH host with password and managed encrypted key. Restart the
  app to confirm Keychain persistence. Refuse a changed host key. Check errors and
  recovery after a failed password or dropped connection.
- Upload/download empty files, large files and nested Unicode folders. Check content
  integrity, progress, cancellation and conflict dialogs. Copy multiple files from
  Finder, paste into SFTP, replace clipboard contents, and ensure stale selections
  do not transfer. Exercise drops, selection, rename, history and confirmed delete.
- Edit a remote file with the built-in editor (`⌘S`, `⌘F`) and a macOS default app;
  verify upload-on-save and cleanup. Verify Open/Show in Finder and terminal links.
- Exercise host folders, OpenSSH/Bitvise imports, key generation/import/export,
  passphrase changes, key installation, broadcast, workspaces and saved SFTP cwd.
- Exercise installed agents, Unix-socket hooks, attention switching, process metrics
  and notifications with permission allowed/denied and the app in the background.
- Review welcome, settings, key manager, palette, lists and multi-pane views at the
  minimum window size, dark/light themes and larger fonts. Check keyboard focus,
  VoiceOver, trackpad scrolling, reduced motion and display changes.
- Offer a newer release: choose the right architecture, reject a wrong checksum,
  retain the running app while the DMG opens, and show clear offline/retry states.

Record untested hardware, external services, accessibility and signing constraints
explicitly in the QA report. Do not equate configured CI with completed CI runs.
