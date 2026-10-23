# Contributing to Tether

Tether is a native Rust/egui SSH, SFTP and local-terminal workspace. Keep macOS
and Windows functional when changing shared code. Native desktop behavior is part
of correctness, including shortcuts, clipboard formats, credential persistence,
startup environment, file dialogs, notifications and install/update flows.

## macOS portability rules

- Keep platform differences in `src/platform.rs` or small `cfg`-gated adapters.
  Keep the SSH/SFTP engine, workspaces, layouts, editors and agents shared.
- Use `Binding::primary` for application shortcuts. Use physical `ctrl` only for
  shell controls or intentionally distinct shortcuts (e.g. Control-Tab). Preserve
  `mac_cmd` independently when recording and serializing bindings. Old bindings
  without this field must still load without changing their physical Ctrl meaning.
- Reserve Command-C/V for clipboard operations; Control-C/D/U/Z must reach local
  and remote shells. Copy with no selection must not interrupt a process on macOS.
  Command shortcuts must never emit terminal bytes. Option-generated Unicode must
  reach the shell as UTF-8 without an unwanted ESC prefix. Test non-US keyboards.
- Derive shortcut hints from effective bindings; use `platform` labels for Finder,
  Keychain and common clipboard gestures. Do not add Windows-only user-facing copy.
- Follow Finder selection and file gestures: Command-click toggles, Shift-click
  ranges, Return renames, Command-Down opens, Command-Up goes to the parent,
  Command-Shift-N creates a folder, Command-Delete requests confirmed deletion.
  Remote deletion is permanent and must retain the existing confirmation dialog.
- Read Finder file selections using AppKit's pasteboard file URLs and change count.
  Text clipboard utilities are not substitutes. Parse file URLs with `url`, retaining
  absolute roots, percent encoding, spaces and Unicode. Never reuse an internal file
  selection after the system clipboard has changed. Preserve drag-and-drop support.
- Enable `keyring`'s `apple-native` backend. Never substitute its mock store or write
  passwords to config files. Keep private keys owner-only, including overwriting
  previously existing exports. Use fresh UUIDs and clean up native Keychain tests.
- Test launching the packaged app from Finder/Dock with a minimal inherited PATH.
  Select the account's login shell, preserve user shell configuration, discover
  Homebrew and per-user tools, and bound subprocess discovery time. Do not mutate
  global process environment after threads have started.
- Support both `aarch64-apple-darwin` and `x86_64-apple-darwin`. macOS system fonts
  include `.ttc` collections and fonts under `~/Library/Fonts` and
  `/System/Library/Fonts/Supplemental`. Keep a bundled font fallback.
- Package with `scripts/package-macos.sh`; keep its architecture-specific DMG names,
  bundle identifier and minimum OS consistent with updater selection and release CI.
  Verify checksums before opening an update; never run an EXE on a Mac or quit while
  asking the user to replace the app. Developer ID signing/notarization is a separate
  release requirement; ad-hoc signing does not establish Gatekeeper trust.
- Shell launch/cleanup, agent hooks over Unix sockets and process monitoring must
  remain functional. Darwin Unix socket paths must fit in 104 bytes: use short
  paths inside private 0700 directories rather than blindly appending to TMPDIR.
  Test the macOS default zsh as well as bash. Do not gate shared
  tests behind `cfg(windows)` to hide portability failures.

## Validation

Run `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`,
`cargo test --locked --lib`, then the real PTY/shell tests and rendered UI tests:

```sh
cargo test --locked --test local_pty --test shell_integration -- --test-threads=1
cargo test --locked --test ui_flow --test ui_layout -- --test-threads=1
```

For SSH/SFTP QA, start the loopback-only disposable server documented in
`tests/e2e_ssh.rs`, then run with `TETHER_E2E=1`. Never treat tests that return early
without that variable as evidence of a real network pass. UI screenshots are in
`target/ui-shots/`. Review them as well as the assertions. Use an isolated
`TETHER_CONFIG_DIR` for manual QA; do not modify a developer's saved sessions.

On an unlocked developer Mac, explicitly test native Keychain persistence:

```sh
cargo test --locked --test macos_native -- --ignored --test-threads=1
scripts/package-macos.sh
```

Before release, complete the manual matrix in `docs/macos.md` on both CPU
architectures. Report exactly what ran, what was skipped, the OS/architecture,
and remaining limits. A successful compile alone does not certify feature parity,
Gatekeeper acceptance, accessibility or UX.
