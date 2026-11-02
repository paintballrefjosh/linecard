# <img src="docs/logo.svg" alt="" width="40" align="absmiddle"> Linecard

[![CI](https://github.com/paintballrefjosh/linecard/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/paintballrefjosh/linecard/actions/workflows/ci.yml)
[![CodeQL](https://github.com/paintballrefjosh/linecard/actions/workflows/codeql.yml/badge.svg?branch=master)](https://github.com/paintballrefjosh/linecard/actions/workflows/codeql.yml)
[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/paintballrefjosh/linecard/badge)](https://scorecard.dev/viewer/?uri=github.com/paintballrefjosh/linecard)
[![Latest release](https://img.shields.io/github/v/release/paintballrefjosh/linecard)](https://github.com/paintballrefjosh/linecard/releases/latest)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

A terminal and remote-desktop client in the spirit of SecureCRT and PuTTY — SSH, Telnet, Serial, RDP and VNC — whose connection tree syncs across your computers with end-to-end encryption. Remote changes show up highlighted (green added, amber edited, red deleted) and any one of them can be reverted.

Windows first; macOS and Linux build from the same code.

## Install

Download `Linecard_<version>_x64-setup.exe` from [Releases](https://github.com/paintballrefjosh/linecard/releases) and run it. It installs for your user only (no admin prompt) and fetches Microsoft's WebView2 runtime if your PC doesn't already have it. Later versions are offered from inside the app.

Builds for macOS (dmg) and Linux (AppImage, deb) are on the same page, unsigned for now: on Windows, SmartScreen warns on the first install ("More info → Run anyway"); on macOS, right-click → Open the first time. To run the very latest code instead, [build it from source](#building-from-source).

**Requirements:** Windows 10 or later (64-bit). Windows 7 and 8 can't run Linecard itself, because Microsoft's WebView2 runtime and current Rust builds no longer support them; Linecard can still connect *to* Windows 7 and Server 2008 R2 hosts over RDP.

You don't need Rust, Node.js or anything else in the next section to use Linecard.

## What works today

| Area | State |
| --- | --- |
| SSH: password, keyboard-interactive, agent (Windows OpenSSH / Pageant), key files including passphrase-protected ones, host-key checks, jump hosts, older algorithms for legacy gear | Done |
| Telnet with option negotiation; Serial (COM ports, BREAK, XMODEM / XMODEM-1K / YMODEM) | Done |
| Connection tree: folders with inherited settings, editor, drag and drop, right-click menus, tab colors | Done |
| Saved passwords in Windows Credential Manager | Done |
| Copy on highlight, right-click paste, confirmation before multi-line pastes | Done |
| Session logging, keyword highlighting, reconnect, saved workspaces, six pane layouts | Done |
| Import from SecureCRT, PuTTY, SuperPuTTY, mRemoteNG and OpenSSH config | Done |
| Local storage as an op log, with per-change revert | Done |
| End-to-end encrypted sync through a shared folder or a self-hosted relay; 24-word pairing phrase; Devices page | Done |
| SSH port forwarding (local, remote, SOCKS); ssh:// and telnet:// links; command-line launcher | Done |
| Keyword-highlight profiles per folder/connection, SecureCRT keyword-list import | Done |
| VNC | Done |
| RDP (IronRDP: NLA, certificate check, resize with the tab, text clipboard both ways, sound) | Done |

## Building from source

Only needed if you want to work on Linecard itself. To just use it, see [Install](#install).

### Prerequisites (Windows)

[Rust](https://rustup.rs), [Node.js 22+](https://nodejs.org), and the Microsoft C++ Build Tools ([Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)). With winget:

```powershell
winget install Rustlang.Rustup OpenJS.NodeJS.LTS
winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

### Run and test

```powershell
cd app
npm install
npm run tauri dev       # desktop app with hot reload
```

UI-only work without Rust: `npm run dev` and open http://localhost:1420. The browser build uses an in-memory mock backend that shows every highlight state, walks through the SSH prompts, and simulates a remote device adding a connection after ~12 seconds.

```powershell
cargo test --workspace
```

### Build an installer

```powershell
cd app
npx tauri build --bundles nsis --config '{\"bundle\":{\"createUpdaterArtifacts\":false}}'
```

The installer lands in `target/release/bundle/nsis/`. The `--config` override skips signing the update files, which needs the release key (see [docs/releasing.md](docs/releasing.md)); CI release builds sign them.

### Working from a network drive

If the repository lives on a mapped network share:

- Incremental Rust builds can fail with "Access is denied" on `target\…\*.rmeta`. Build to a local folder: `$env:CARGO_TARGET_DIR = "$env:USERPROFILE\.cache\linecard-target"`.
- Vite's file watcher doesn't work on network shares. Set `$env:LINECARD_POLL = "1"` before `npm run dev` or `npm run tauri dev`.

### Layout

```
app/               Desktop app: React UI (src/) + Tauri shell (src-tauri/)
crates/
  core-model       Folders, connections, inheritance
  core-sync        Clocks, ops, merge, change tracking
  core-store       SQLite persistence
  proto            Session trait + SSH, Telnet, Serial drivers, XMODEM/YMODEM
  importers        SecureCRT, mRemoteNG, SuperPuTTY, PuTTY, ssh_config
  vault            Credentials (OS keychain, optional encrypted sync)
relay/             Self-hosted sync relay (Docker; see docs/relay.md)
docs/              Roadmap, sync protocol, releasing
```

## Maintainer setup

Releases are built by GitHub Actions from a version tag; the one-time update-signing key setup and the release steps are in [docs/releasing.md](docs/releasing.md).

## Security

Report vulnerabilities privately; see [SECURITY.md](SECURITY.md). Every change runs clippy, the tests on Windows, Linux and macOS, `cargo-deny`, `npm audit` and CodeQL; the OpenSSF Scorecard and Dependabot run weekly. Update files and relay images are signed.

## License

Dual-licensed under [MIT](LICENSE-MIT) or Apache-2.0, at your option.
