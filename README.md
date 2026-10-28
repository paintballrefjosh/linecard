# Linecard

A terminal and remote-desktop client in the spirit of SecureCRT and PuTTY — SSH, Telnet, Serial, RDP and VNC — whose connection tree syncs across your computers with end-to-end encryption. Remote changes show up highlighted (green added, amber edited, red deleted) and any one of them can be reverted.

Windows first; macOS and Linux build from the same code.

## Status

Phase 1 scaffold. What works today:

| Area | State |
| --- | --- |
| Object model: folders, connections, inherited settings | Done, tested (`crates/core-model`) |
| Sync engine core: HLC clocks, op log, per-field merge, revert, change tracking | Done, tested (`crates/core-sync`) |
| SSH driver: password, keyboard-interactive, agent, key files, host-key checks, legacy algorithms | Done, tested end to end (`crates/proto/tests/ssh.rs`) |
| Telnet driver with option negotiation (NAWS, TTYPE, ECHO, SGA) | Done, tested (`crates/proto`) |
| SecureCRT importer | Done, tested (`crates/importers`) |
| Local SQLite store | Written, not yet wired into the app |
| UI: tree with change highlighting + hover cards, tabs, six layouts, themes, Devices page, quick connect, updater | Written; runs against a mock backend in the browser |
| Serial, RDP, VNC drivers; sync transports; relay accounts | Next — see [docs/roadmap.md](docs/roadmap.md) |

## Layout

```
app/               Desktop app: React UI (src/) + Tauri shell (src-tauri/)
crates/
  core-model       Folders, connections, inheritance
  core-sync        Clocks, ops, merge, change tracking
  core-store       SQLite persistence
  proto            Session trait + protocol drivers
  importers        SecureCRT, mRemoteNG, SuperPuTTY, PuTTY, ssh_config
  vault            Credentials (OS keychain, optional encrypted sync)
relay/             Self-hosted sync relay (Docker)
docs/              Plan, sync protocol, releasing
```

## Getting started (Windows)

Prerequisites: [Rust](https://rustup.rs), [Node.js 22+](https://nodejs.org), and the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) (Microsoft C++ Build Tools and WebView2, which Windows 11 already has).

```powershell
cd app
npm install
npm run tauri dev       # desktop app with hot reload
```

UI-only work without Rust: `npm run dev` and open http://localhost:1420. The browser build uses an in-memory mock backend that shows every highlight state and simulates a remote device adding a connection after ~12 seconds.

Tests:

```powershell
cargo test --workspace
```

Release build: `npm run tauri build` → installer in `target/release/bundle/nsis/`.

## First-time repo setup

1. Create `github.com/<you>/linecard` (the `linecard` GitHub org is taken by an unrelated project) and replace `OWNER` in `Cargo.toml` and `app/src-tauri/tauri.conf.json`.
2. Run `npm install` in `app/` and commit `app/package-lock.json` and `Cargo.lock` — CI uses them.
3. Add `LICENSE-APACHE` (GitHub's "Add file → Create new file → LICENSE-APACHE → Choose a license template → Apache 2.0").
4. Generate the update-signing key and add the repo secrets — see [docs/releasing.md](docs/releasing.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or Apache-2.0, at your option.
