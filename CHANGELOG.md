# Changelog

## Unreleased

**Keyword highlighting**
- Highlight profiles (e.g. Cisco, Linux, General): pick one per folder or connection, inherited like the tab color, with an app-wide default. Profiles sync between computers.
- Rules can be whole words or regular expressions, with theme colors or any custom color; optional match case per profile. Earlier rules win where matches overlap.
- Import SecureCRT keyword lists (Config\Keywords\*.ini), with their colors and regular expressions.

**SSH port forwarding**
- Per connection: local (-L), remote (-R) and dynamic SOCKS 4/5 (-D) forwards, started when the session logs in. The terminal shows each tunnel and whether it is working; a port that is already taken fails on its own without stopping the others.

**Links and command line**
- `linecard spine01`, `linecard "DC1/Spine/spine01"`, `linecard ssh admin@host -p 2222`, `linecard telnet cs1 2005`, `linecard serial COM3 9600`: opens a tab in the running window, or starts Linecard.
- Settings → Links and command line: open ssh:// and telnet:// links in Linecard (Windows, per user). A handler that was there before is put back when you turn it off.

**Folders**
- Folder settings (right-click a folder): username, SSH jump host, keyword highlighting, session logging and tab color for everything inside.

**Sync**
- Settings → Sync: start a sync group or join one with its 24-word phrase. Connections, folders and app-wide settings stay the same on every computer; changes from other computers are highlighted and can be reverted one by one.
- Sync through any shared folder (NAS share, OneDrive, Dropbox, Syncthing) or a self-hosted relay (`relay/`, Docker image).
- End-to-end encrypted: XChaCha20-Poly1305 with the group key, each batch signed by its device; the folder or relay only stores ciphertext. Sync keys live in the OS keychain.
- The Devices page lists every computer in the group with its app version and last check-in; the status bar shows sync status and changes waiting to be sent.

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
