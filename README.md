# tether

SSH terminals and fast SFTP side by side, in one native Rust window.

![tether](docs/screenshot.png)

A personal project: the split-pane workspace of Termius plus the SFTP speed of Bitvise, with no Electron and no account. Built with [egui](https://github.com/emilk/egui) and [russh](https://github.com/Eugeny/russh).

## Features

- **Split workspace**: drag any terminal or SFTP tab into any split layout. Saved workspaces, broadcast input to several terminals.
- **Fast SFTP**: one SSH connection is shared by every tab, and transfers keep up to 4 MB of requests in flight. That's about 2–3× faster than a plain client at 20 ms latency.
- **Hosts**: nested folders, drag and drop, archive, search and quick connect (`user@host:port`). Imports hosts from `~/.ssh/config` and Bitvise.
- **Key manager**: generate, import and export keys, add or change a passphrase, and install a key on a server in one click.
- **SFTP that remembers**: each host reopens in its last folder, and *Go to terminal folder* follows the shell next to it.
- **Secure by default**: secrets live in the OS credential store, host keys are trusted on first use, and tether refuses to connect if a host key changes.
- **Customisable**: themes, and every shortcut can be rebound (*Settings → Keyboard*).

### Terminal and agent shortcuts

- **Shift+Enter** inserts a line without submitting in Claude Code, Codex and integrated PowerShell, Bash, Zsh and Fish prompts. Enter submits normally.
- **Images**: a local terminal running a detected Claude Code or Codex session shows **+ Image** and accepts screenshot / PNG / JPEG attachments. Ordinary shells use the full terminal height and do not show the image toolbar. Colored **[Image 1]** labels show a thumbnail on hover; Tether inserts a usable file path for the agent. Local temporary copies are removed when the terminal closes. **Clear previews** only hides the thumbnails; edit the prompt in the terminal to remove an inserted path.
- **Claude Code / Codex**: Codex has its own movable sidebar view with terminal sessions, activity, attention badges, CPU / memory, last messages and navigation, using the same monitoring system as Claude. Configure Claude hooks and Codex completion notifications in **Settings → AI agents**. Codex `notify` reports completed turns; ongoing work is tracked from terminal activity and processes.
- **Custom Boot**: right-click either agent icon, enter your local command and choose its shell. Tether passes the command and flags through unchanged, including account selectors implemented by your own functions or aliases. Choose the shell whose startup profile defines them (Windows PowerShell and PowerShell 7 use different profiles). The shell is remembered separately for each agent.
- **Project launchers**: both normal launch and Custom Boot ask for a local project directory in the same card. Type a path (including `~`), use Tab / Shift+Tab completion or browse. Give it a name and choose **☆ Save** to pin a shortcut shared by Claude and Codex. Local terminal folders are remembered automatically, with up to 32 recent folders; right-click a shortcut to pin, unpin or forget it.
- **Terminal colors**: local PTYs and SSH sessions share `xterm-256color` / true-color capability hints. Local shells do not inherit a GUI runner's `NO_COLOR`; ANSI, indexed and RGB colors use the existing terminal renderer and theme palette. SSH servers may restrict optional environment hints through `AcceptEnv`; their PTY still advertises 256 colors.
- **Remote path links**: click a remote path to open SFTP beside that terminal, using the same companion pane as **Ctrl+Shift+F**; right-click also offers **Open in SFTP**, the editor and copy options. Repeated clicks navigate the existing pane. Paths inside prompts such as `prisma@host:~/project$` retain the prompt's account: `~` resolves to prisma's actual home on the server, even when SSH logged in as root and the shell used `su` / `sudo`. No `/home/<user>` assumption is made. Relative paths resolve against the source terminal's directory; files open their containing folder.
- **Tab close targets**: hovering the close button or its surrounding area keeps the tab's current expansion, so resource details do not move the X while you aim for it.

### Share machine setups and find SSH keys

Open **Import / export setup…** from the app or Hosts menu. **Export setup** previews the machines and explains that anyone with the JSON file can import their names, IP addresses / hostnames, ports, usernames and colors. Keys, key paths, passwords and passphrases are excluded. Choose the **Tether setup** source card under **Import**, review the selected machines and import them; existing endpoints are skipped and imported machines need credentials before connecting.

**Search this PC** in Import finds Bitvise profiles in common OS-provided folders. **Search this PC for SSH keys…** in the key manager or host editor searches `.ssh`, Bitvise locations, Documents, Desktop and Downloads, and displays each result's full path. Searches run in the background with a depth/time/file limit and skip symbolic links. You choose what to import; **Browse…** handles other locations. Export keys held in Bitvise's encrypted internal store as OpenSSH files from Bitvise before importing them.

## Install

Download `Tether-Setup-<version>.exe` from [Releases](https://github.com/izLoki/Tether/releases) and run it. No admin rights needed. It adds Start menu and desktop shortcuts. Tether checks for new releases on startup and installs them in one click (*Settings → About → Updates*). Running a newer installer also upgrades in place.

On **macOS**, download the `Tether-<version>-macos-<arch>.dmg` for Apple Silicon
(`aarch64`) or Intel (`x86_64`), then drag Tether to Applications. macOS builds use
Command shortcuts, Keychain, Finder file copying and native login shells. Packages
currently use ad-hoc signing; Developer ID signing/notarization is still needed for
Gatekeeper-trusted distribution. See [macOS support and QA](docs/macos.md).

Windows and macOS have automated checks. Linux remains untested.

### Build from source

```sh
git clone https://github.com/izLoki/Tether
cd Tether
cargo build --release --locked   # Windows: target/release/tether.exe; macOS: target/release/tether
# macOS app bundle + disk image:
scripts/package-macos.sh
```

Requires Rust 1.95+ (edition 2024), matching egui/eframe. macOS also needs Xcode Command Line Tools.

## Develop

```sh
cargo test                               # unit + headless UI tests (screenshots in target/ui-shots/)
TETHER_E2E=1 cargo test -- --test-threads=1   # against a real server, see tests/e2e_ssh.rs
```

To cut a release, bump `version` in `Cargo.toml`, commit, then run `git tag v0.1.2 && git push --tags`. GitHub Actions builds the installer (`installer/tether.iss`, Inno Setup) and publishes it with its `.sha256` and a portable exe. macOS jobs build Apple Silicon and Intel app bundles and publish
DMGs with SHA-256 checksums. The icon comes from `python assets/make_icon.py`.

Config lives in `%APPDATA%\tether\tether\config\` on Windows and
`~/Library/Application Support/dev.tether.tether/` on macOS. Set `TETHER_CONFIG_DIR` to move it.

## License

[MIT](LICENSE)
