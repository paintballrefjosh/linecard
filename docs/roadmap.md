# Roadmap

Full plan: the "Linecard — Project Plan" doc. Each phase ends in a usable release.

## Phase 1 — Core terminal (v0.1–0.3)

- [x] Object model with folder inheritance
- [x] Sync engine core (clock, ops, merge, revert, change tracking)
- [x] Telnet driver
- [x] SecureCRT importer
- [x] UI shell: tree, tabs, six layouts, themes, Devices page, quick connect, updater hook
- [x] Wire `core-store` into the app: every edit is an op in SQLite; the tree is projected from the replica; stable device id
- [x] SSH driver (`russh`): password, keyboard-interactive, agent (Windows OpenSSH / Pageant), unencrypted key files, host-key verification with known_hosts, legacy algorithms for older gear
- [x] SSH: passphrase-protected keys (asked only when the server takes the key), jump hosts (chains, inherited from folders)
- [x] Reconnect: Enter (or the terminal menu) reconnects a dropped session in the same tab
- [x] Serial driver (`serialport`): port picker with live ports, framing, flow control, BREAK
- [x] XMODEM / XMODEM-1K / YMODEM send and XMODEM receive over serial, with progress and cancel
- [x] Connection editor dialog
- [x] Show inherited values (and where they come from) in the connection editor
- [x] Credential vault: saved passwords in the OS keychain (Windows Credential Manager), "Save password" on login prompts, list and remove in Settings
- [x] Session logging (plain text, optional timestamps, global/folder/connection settings) and keyword highlighting (profiles per folder/connection, SecureCRT import)
- [x] Importers: SecureCRT, PuTTY (registry, / folders), mRemoteNG (confCons.xml), OpenSSH config; import dialog with preview
- [x] SuperPuTTY Sessions.xml (folders from session paths)
- [x] Saved workspaces: open tabs and layout saved by name, restored with reconnects

## Phase 2 — Sync (v0.4–0.6)

- [x] Encryption, pairing phrase, device keys (QR: later)
- [x] Relay: SQLite, per-group tokens, quotas, Docker image
- [x] Relay: instant updates (long-poll), accounts for private relays, admin CLI
- [x] Shared-folder transport (NAS, OneDrive, Dropbox, Syncthing)
- [x] GitHub transport (30 s poll)
- [ ] LAN peer transport
- [x] Device heartbeats and version-mismatch banners
- [x] Remove a device / change the phrase (key rotation)
- [ ] Optional encrypted secrets sync
- [x] Port forwarding (local, remote, SOCKS), ssh:// and telnet:// links (Windows), command-line launcher

## Phase 3 — Graphical (v0.7–0.8)

- [ ] RDP: native Windows control on Windows, IronRDP elsewhere
- [ ] VNC

## Phase 4 — Power tools (v0.9–1.0)

- [ ] Broadcast input, button bar / snippets, SFTP browser
- [ ] Python scripting (embedded CPython via PyO3)
- [ ] macOS and Linux hardening; winget and Homebrew

Team sharing follows 1.0.
