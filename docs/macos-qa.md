# macOS portability QA — 2026-10-20

Branch: `MacOS-Portability`. Base: `969e3e4` (`main`, Tether 0.2.2).
Host: macOS 27.2 (26B5101f), Apple Silicon, Rust 1.99.1.
The x86_64 binaries were compiled for `x86_64-apple-darwin` and executed using
Rosetta on the same Mac. This is not a physical Intel hardware certification.

## Changes verified

- Real macOS Keychain persistence, independent Command/Control bindings, lossless
  shortcut recording and compatibility with old physical Control overrides.
- Native terminal/tab/split/settings shortcuts, Finder selection and file gestures;
  new-host uses Command-N so Command-Shift-N remains available for SFTP folders.
- Control-C/V/X bytes reach macOS shells; Command-C without selection never sends
  SIGINT; Option Unicode text is not prefixed with ESC. Paste sanitization remains.
- Native Finder file-URL pasteboard handling and change counts; file-URL parsing
  preserves absolute roots, spaces, percent encoding and Unicode. Changed system
  clipboard contents no longer fall back to a stale internal file selection.
- Account login shell and bounded shell PATH discovery, Homebrew/per-user paths,
  bash/zsh command blocks, working directory, exit codes and local PTY lifecycle.
- Short Darwin-compatible Unix-socket paths in private directories for agent hooks;
  actual hook-to-inbox round trips, process sampling and state logic pass.
- macOS font locations, Mac Roman names and regular faces in TTC files; a saved
  face index is loaded correctly, with bundled-font fallback. Existing exported
  private key files are restricted to mode 0600.
- Architecture-specific DMG update selection and checksum requirements. Windows
  installer parser fixtures still run on macOS. The Mac updater leaves the app
  running and reports failures from `open`.
- A real-server test exposed an SSH close that could wait indefinitely for a remote
  ChannelClose response. Session shutdown now also observes writer completion;
  the real tests bound shutdown time so this regression cannot silently hang QA.
- Settings controls stack at narrow widths, navigation scrolls independently above
  the footer, and navigation buttons expose accessible names. These changes fix
  overlaps found in 760×480 and Retina screenshot review.

## Results

| Check | Apple Silicon | Intel binary via Rosetta |
| --- | --- | --- |
| Library tests | 364 passed, 8 intentionally ignored | 364 passed, 8 intentionally ignored |
| Real local PTY | 2 passed | 2 passed |
| Real bash + zsh integration | 2 passed | 2 passed |
| OpenSSH/SFTP fixture | 9 real network tests passed | 9 real network tests passed |
| UI flow + layout rendering | 2 passed | 2 passed |
| Native Keychain CRUD | 1 explicitly invoked test passed | Not separately invoked |
| Formatting + strict Clippy | Passed | Native host lint; not a separate x86 lint run |
| Release app + DMG | Built and verified | Release build delegated to configured CI |

The E2E binary reports 10 passing tests: `bench_pipeline_sizes` returns early unless
`TETHER_BENCH` is set, so it is **not** counted as network or performance coverage.
The eight ignored library tests are optional benchmarks/screenshots and were not
counted as functional passes. There were no remaining failures in the runs above.

The real server was a disposable `linuxserver/openssh-server` container listening
only on `127.0.0.1:2222`. Network tests exercised authentication failure, shared
transport reuse, terminal resize/close/cwd, encrypted managed-key installation and
authentication, recursive file operations, empty files, hostile names, preservation
of existing local files after failed downloads and byte-exact 48 MiB-plus transfers
with serial and pipelined requests.

Headless UI runs produced 35 PNGs per architecture. The flows cover host validation,
imports, key generation/details, host/folder/workspace menus, local shells, palette,
SSH/SFTP panes, splits, view movement, panel docking, maximization and Settings.
Minimum-size rendering uses 760×480 logical points; the Retina case explicitly
sets native display density to 2 and asserts that effective density remains 2.
Representative welcome, palette, SSH/SFTP, split and small-window images were
visually reviewed; no claim of exhaustive pixel-by-pixel review is made.

Native application smoke testing additionally observed Command-T, Command-comma,
Command-D, Command-W and the macOS application menu. With an explicitly minimal
parent PATH and isolated `TETHER_CONFIG_DIR`, the packaged app started zsh and
resolved Homebrew. A running `sleep` continued after Command-C and returned to the
prompt after physical Control-C. Native Quit/reopen and full workspace restoration
are not certified by this smoke run.

Packaging checks include `lipo -verify_arch`, Info.plist validation, strict ad-hoc
code-signature verification, SHA-256 generation and a readable/mountable disk image.
The CI/release workflow YAML and the packaging shell syntax were validated locally.
CI now targets Windows, Apple Silicon macOS and Intel macOS; those remote jobs have
been configured, **not executed from this local branch**.

## Reproduce

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
TETHER_E2E=1 cargo test --locked --lib --test local_pty --test shell_integration --test e2e_ssh --test ui_flow --test ui_layout -- --test-threads=1
TETHER_E2E=1 cargo test --locked --target x86_64-apple-darwin --lib --test local_pty --test shell_integration --test e2e_ssh --test ui_flow --test ui_layout -- --test-threads=1
cargo test --locked --test macos_native -- --ignored --test-threads=1
scripts/package-macos.sh
```

See `tests/e2e_ssh.rs` for the disposable server command. Avoid running the two
architecture suites concurrently against the same fixture: some remote test paths
are deliberately fixed. Their screenshots share `target/ui-shots`, so retain each
architecture's images before starting the other run.

## Release limitations and follow-up

1. The DMG has an ad-hoc signature, not Developer ID signing or notarization.
   Gatekeeper acceptance of public, quarantined downloads is not certified.
2. A physical Intel Mac, the declared oldest supported macOS (11), and Windows were
   not available for local runtime QA. Validate those through CI and release testing.
3. Finder multi-file copying/dropping, external-app remote editing, notification
   permission states, real installed agent CLIs, IMEs, VoiceOver, multi-monitor
   detachment and native quit/reopen need the manual release matrix in `macos.md`.
   Automated coverage validates the underlying adapters and shared logic, not every
   OS permission or desktop interaction.
4. A live upgrade from an upstream macOS release could not run because those assets
   are introduced by this contribution. Platform/architecture selection, required
   checksums and rejected untrusted sources are covered by local parser tests.

[CLAUDE.md](../CLAUDE.md) records the portability rules and required future checks.
