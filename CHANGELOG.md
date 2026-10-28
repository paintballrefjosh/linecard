# Changelog

## v0.1.0 — first release

The core terminal, for Windows (macOS and Linux builds are included but less tested).

**Sessions**
- SSH: password, keyboard-interactive, Windows OpenSSH agent / Pageant, key files (passphrase-protected ones only ask when the server accepts the key), host-key verification with known_hosts, jump hosts (chains, set per connection or folder), older algorithms for legacy network gear.
- Telnet with option negotiation; Serial consoles with BREAK and XMODEM / XMODEM-1K / YMODEM transfers.
- Reconnect with Enter; per-tab login and host-key prompts; duplicate sessions numbered "(2)", "(3)"; activity dots on background tabs.

**Connections**
- Folder tree with inherited settings (username, jump host, tab color, logging), shown in the editor with where each value comes from.
- Right-click menus, inline rename, drag and drop, tab colors on tabs and in the tree.
- Saved passwords in Windows Credential Manager.
- Import from SecureCRT, PuTTY, SuperPuTTY, mRemoteNG and OpenSSH config.
- Stored as an op log in SQLite (the groundwork for sync and per-change revert).

**Terminal**
- Copy on highlight, right-click paste, confirmation before multi-line pastes.
- Keyword highlighting with editable rules; session logging to plain text.
- Six pane layouts, saved workspaces, light/dark/system theme, resizable sidebar.

**Updates**
- The app checks GitHub Releases and offers "Restart to install" for new versions.
