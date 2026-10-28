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
- [ ] SSH: passphrase-protected keys, jump hosts, reconnect
- [x] Connection editor dialog
- [ ] Show inherited values in the connection editor
- [ ] Credential vault on the OS keychain
- [ ] Session logging and keyword highlighting
- [ ] mRemoteNG, SuperPuTTY, PuTTY, ssh_config importers + import preview wizard
- [ ] Saved workspaces

## Phase 2 — Sync (v0.4–0.6)

- [ ] Encryption, pairing phrase/QR, device keys
- [ ] Relay: accounts and tokens (multi-user), WebSocket push, SQLite, quotas, admin CLI
- [ ] GitHub transport (30 s default poll), LAN peer transport
- [ ] Device heartbeats and version-mismatch banners
- [ ] Optional encrypted secrets sync
- [ ] Port forwarding, URL handlers, CLI

## Phase 3 — Graphical (v0.7–0.8)

- [ ] RDP: native Windows control on Windows, IronRDP elsewhere
- [ ] VNC

## Phase 4 — Power tools (v0.9–1.0)

- [ ] Broadcast input, button bar / snippets, SFTP browser
- [ ] Python scripting (embedded CPython via PyO3)
- [ ] macOS and Linux hardening; winget and Homebrew

Team sharing follows 1.0.
