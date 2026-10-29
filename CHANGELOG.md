# Changelog

## Unreleased

**Graphical sessions**
- RDP: connections of type RDP open in a tab (IronRDP, on every platform). Network Level Authentication (CredSSP/NTLM) with the user name inherited like SSH and "Save password"; the server certificate is checked like an SSH host key before any password is sent; the remote desktop follows the tab's size when the server supports it.
- VNC: connections of type VNC open in a tab showing the remote screen, scaled to fit, with mouse, wheel and keyboard (Ctrl+Shift+V types the clipboard). VNC password authentication with "Save password"; ZRLE, CopyRect and Raw encodings; servers that change size.

- Sync saved passwords (opt-in, Settings → Saved passwords): encrypted with a vault passphrase that is separate from the sync phrase (Argon2id, then XChaCha20-Poly1305 per password). Each computer asks for the passphrase once; saving, changing or removing a password on one computer does the same on the others.
- Sync on the local network with no server: pick "This network" in Settings → Sync. Computers find each other by multicast and sync directly; each keeps and passes on what it has, so changes reach a computer that was off when they were made.
- Sync through a GitHub repository: pick GitHub in Settings → Sync, enter owner/name and a fine-grained token for that repository (Contents: read and write; kept in the keychain). Same layout as a shared folder, one commit per batch, checked every 30 seconds; the repository only holds ciphertext.
- Relay sync is near-instant: computers wait on the relay, which answers as soon as a change arrives (about a quarter second end to end), instead of checking every 5 seconds.
- Private relays: with LINECARD_RELAY_OPEN=false only accounts can start sync groups. The operator creates them with `linecard-relay account add`, which prints an access key to enter in Settings → Sync. Admin commands also list and remove groups and accounts (see docs/relay.md).
- Settings → Links and command line: "Add linecard to PATH" (Windows: your user PATH; Linux: ~/.local/bin; macOS shows the one-line command, which needs admin rights).
- ssh:// and telnet:// links can open in Linecard on macOS and Linux too (Launch Services / xdg-mime), and the handler they replace comes back when turned off.
- Drag and drop works in the desktop app: move connections and folders in the tree, and drag tabs between panes. (The window was claiming drags for file drops.)
- Dragging an item further down in the same folder no longer lands it one place too far.

## v0.2.0 — sync, port forwarding, highlight profiles

**Keyword highlighting**
- Highlight profiles (e.g. Cisco, Linux, General): pick one per folder or connection, inherited like the tab color, with an app-wide default. Profiles sync between computers.
- Rules can be whole words or regular expressions, with theme colors or any custom color; optional match case per profile. Earlier rules win where matches overlap.
- Import SecureCRT keyword lists (Config\Keywords\*.ini), with their colors and regular expressions.

**SSH port forwarding**
- Per connection: local (-L), remote (-R) and dynamic SOCKS 4/5 (-D) forwards, started when the session logs in. The terminal shows each tunnel and whether it is working; a port that is already taken fails on its own without stopping the others.

**Links and command line**
- `linecard spine01`, `linecard "DC1/Spine/spine01"`, `linecard ssh admin@host -p 2222`, `linecard telnet cs1 2005`, `linecard serial COM3 9600`: opens a tab in the running window, or starts Linecard.
- Settings → Links and command line: open ssh:// and telnet:// links in Linecard (Windows, per user). A handler that was there before is put back when you turn it off.

**Updates**
- macOS builds now include the update package, so Macs get "Restart to install" like Windows and Linux (from the release after this one).

**Folders**
- Folder settings (right-click a folder): username, SSH jump host, keyword highlighting, session logging and tab color for everything inside.

**Sync**
- Settings → Sync: start a sync group or join one with its 24-word phrase. Connections, folders and app-wide settings stay the same on every computer; changes from other computers are highlighted and can be reverted one by one.
- Sync through any shared folder (NAS share, OneDrive, Dropbox, Syncthing) or a self-hosted relay (`relay/`, Docker image).
- End-to-end encrypted: XChaCha20-Poly1305 with the group key, each batch signed by its device; the folder or relay only stores ciphertext. Sync keys live in the OS keychain.
- Remove a computer from the Devices page, or change the phrase in Settings → Sync: Linecard makes a new phrase, seeds the new group with the full history, and tells the other computers to enter the new phrase (the removed one gets no new changes).
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
