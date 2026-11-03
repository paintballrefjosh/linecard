# Changelog

## Unreleased

- **RDP: share a folder with the remote computer (Windows).** In an RDP connection's settings, **Shared folder** picks one folder on this computer to appear as a drive in the remote session. It is read-only unless you tick "Let the remote computer change and delete files in it". The choice is kept on this computer only and is not synced to your others, since a folder path means nothing elsewhere and a synced setting could expose a folder here. On macOS and Linux the setting explains that it is not available yet. Windows 10 hosts needed two small fixes to the sharing library, kept in `vendor/` (see its README).

- **VNC: Tight encoding**, so TightVNC servers (common on Windows, and without the ZRLE encoding Linecard already used) send compressed screen updates instead of falling back to raw pixels. JPEG-quality Tight is not requested; updates stay lossless.

- **The Windows installer no longer ticks "Create desktop shortcut" by default.**

- **Right-click a tab to Reconnect or Close it.** Reconnect works on terminal, RDP, VNC and SFTP tabs once they have disconnected.
- **A disconnected RDP or VNC tab now blanks the frozen picture** and shows the "Disconnected" message with a Reconnect button, instead of leaving the last image on screen.

- **RDP sound**: the remote computer's sound plays on this one, with a mute button in the corner of the tab (the server-to-you direction only; no microphone). Windows hosts only start it when the client offers the device-sharing channel, so Linecard now always does (with nothing shared unless you pick a folder). Tested against one Windows 10 host.

- **VNC over TLS (VeNCrypt)**: servers that insist on encryption, which is TigerVNC's and most Linux servers' default, now work, with a VNC password or a username and password. The server's certificate is shown the first time (like an SSH host key) and remembered if you accept it; a changed certificate warns you. Servers that offer only anonymous TLS (no certificate) are still unsupported.

- **F5, Ctrl+R and the right-click Reload no longer reload the window**, which used to close every open connection. Terminal programs still receive the keys.

- **Windows installer upgrades in place**: running a newer installer over an installed Linecard no longer asks whether to uninstall the old version first; it just updates it, keeping your connections and settings.

## v0.4.0 — RDP clipboard and pointers, bulk editing, Mac Screen Sharing, scripting on a schedule, relay compaction and WebSocket push

- **Scripts: several at once, and on a schedule**: up to six scripts can run at the same time (each with its own output tab in the Scripts window), and **Run on a schedule** starts a script every N minutes or daily at a set time while Linecard is open. See `docs/scripting.md`.

- **SFTP: drag files in**: drop files or whole folders from Explorer onto an SFTP tab to upload them to the folder you are viewing, with the same Cancel and Resume as the Upload buttons. (The window gives dropped files no path, so they are copied to a temporary folder first and removed afterwards; very large files need that much free space in your temp folder.) Dragging files out to the desktop is not possible in this window; use Download.

- **VNC: Mac Screen Sharing and pointer shapes**: connecting to a Mac's built-in Screen Sharing (Apple Remote Desktop login: your Mac username and password) now works, where it used to stop at "security type not supported". The VNC pointer also follows the server's shape (I-beam, resize arrows, hidden). Connections that have a username set use the Mac account login when the Mac offers both. Tested against a built-in server in this repo, not against a real Mac.

- **RDP pointer shapes**: the mouse pointer over an RDP screen now follows the server's: the text I-beam over text, the resize arrows on window edges, the busy ring, a hidden pointer in games and full-screen video. Before, it was always the local arrow.

- **Password sync, two computers at once**: if two computers turned password sync on at the same moment, each made its own vault and one passphrase won. The other now says so plainly and asks for the winning passphrase (before, it showed a generic error), and when it does, its saved passwords are re-saved under the winning vault instead of being left unreadable. Records nothing can read any more are cleaned up.

- **Multi-select**: Ctrl/Shift-click connections, then press Enter or right-click → **Open N in tabs** to open them all, or **Edit N connections…** to change settings on all of them in one window. The window shows only what can be shared (port, jump host, username, session log, keyword highlighting, tab color, notes, and serial framing when all are serial); a setting that differs between them reads "Mixed" and is left alone unless you change it. Names and hosts are never touched.

- **RDP clipboard**: copy and paste text between your computer and an RDP session, in both directions (when the tab gains focus, as with VNC). Files and images are not shared.

- **Fixes**: the change-details popup stays inside the window (it moves up or to the left of the row instead of running off the edge, and long values wrap); searching with no results says "No connections match …" instead of showing an empty pane; Ctrl+Tab and Ctrl+Shift+Tab leave the keyboard in the session; after saving a connection it is selected and focused in the tree, so Enter opens it; clicking a tab puts the keyboard in the session instead of on the tab; the update notice now appears in a running app (checked hourly and when you return to the window) instead of only after a restart.

- **Security checks**: CodeQL, OpenSSF Scorecard, dependency review on pull requests, weekly Dependabot updates, and a `SECURITY.md` policy; badges in the README.

- **Relay WebSocket push**: computers keep a WebSocket open to the relay and hear about new batches immediately, instead of holding a request open and re-asking every 25 seconds. A proxy or older relay that doesn't allow WebSockets makes Linecard fall back to the old way automatically.

- **Relay compaction**: a sync group on a relay no longer grows forever. When it holds 500 or more batches, one caught-up computer replaces the old history with a snapshot of the current state (the relay only accepts one that is smaller, and stays unable to read it). Every computer, including ones behind and ones that join later, converges to the same library; the only loss is reverting a change that was compacted before a computer saw it. Needs a relay from this version; with an older relay nothing happens. Design in `docs/sync-protocol.md`.

- **Scripting, more ways in**: `linecard run script.py` (or the name of a script in the scripts folder) runs a script in the running window; **Run on connect** in the Scripts window starts a script whenever a chosen connection logs in; and `session.screen()` returns the text on the terminal screen right now.

- **SFTP**: a Cancel button for transfers, **Resume** after a cancel or failure (continues the files already partly copied), and a Permissions button (read/write/execute grid or octal number, optionally for everything inside a folder).

- **Sync phrase QR code**: Settings → Sync → Show phrase → **QR code**, to scan the phrase into a phone's password manager.

- **Relay: key rotation, direct HTTPS, signed images**: `linecard-relay account rotate <name>` replaces an account's access key (the old one stops working; the account's groups keep syncing). `LINECARD_RELAY_TLS_CERT_FILE` and `LINECARD_RELAY_TLS_KEY_FILE` make the relay serve HTTPS itself, with no proxy, and pick up renewed certificates without a restart. Released relay images are now signed with cosign (keyless); verification steps are in `docs/relay.md`.

- **Dependency audit in CI**: every push and a weekly run check Rust dependencies for known vulnerabilities, licenses and sources (`cargo-deny`, policy in `deny.toml`) and the UI's for npm advisories. Six findings in upstream crates (IronRDP, Tauri's GTK stack) with no fix available are listed in `deny.toml` with reasons; `npm audit` is clean.

- **Python scripting**: the **Scripts** button in the toolbar runs Python files that drive your open sessions: `s = lc.active()`, `s.send_line("show version")`, `s.expect("#")`, `lc.open("Lab/spine01")` to open a saved connection and wait for it, `lc.ask(...)` to ask you for a password. Output shows in the window, Stop ends the script. Scripts run in your own Python 3.8+ (path in Settings → Scripting) as a separate process, so they can't crash Linecard, and they never see saved passwords. See `docs/scripting.md`.

- **Relay protection and visibility**: request rate limits per client address and per sync group (`LINECARD_RELAY_RATE`, with `Retry-After`, which Linecard now obeys), a cool-down for addresses that keep sending wrong tokens, caps on held-open requests and a 30 s limit on everything else; `LINECARD_RELAY_TRUST_PROXY` for use behind a proxy; one structured log line per request (`LINECARD_RELAY_LOG`, `LINECARD_RELAY_LOG_FORMAT=json`) with no tokens or full group ids; Prometheus `/metrics` on its own port (`LINECARD_RELAY_METRICS_ADDR`); `/ready`; and `linecard-relay config`, which lists every setting.

- **Relay backups**: `linecard-relay backup <file or folder>` copies the database while the relay serves and checks the copy; `linecard-relay restore <file>` replaces it (stop the relay first; the old file is kept). `LINECARD_RELAY_BACKUP_DIR` and `LINECARD_RELAY_BACKUP_KEEP` make a daily rotating backup, which the Compose bundle turns on. A restored relay announces a new history id, and Linecard re-sends its changes and re-reads the relay, so a restore does not strand computers that were ahead of it.

- **Relay retention**: groups are capped by size as well as batch count (`LINECARD_RELAY_MAX_GROUP_BYTES`, 256 MiB by default); computers unseen for 90 days leave the device list; `LINECARD_RELAY_IDLE_DAYS` can delete abandoned groups; `linecard-relay vacuum` returns disk space; the database now carries a schema version and upgrades itself. `group list` shows sizes and last activity, and `group list --idle N` shows quiet groups.

- **Relay deployment bundle** (`relay/deploy`): `docker compose up -d` runs the relay behind Caddy with automatic HTTPS, with notes for nginx, Traefik, Cloudflare Tunnel, plain Docker and systemd.

- The relay image is now a small static build on a distroless base for amd64 and arm64, runs as a non-root user, reports its own health (`linecard-relay health`) and stops gracefully on `docker stop`. Image tags are now `:X.Y.Z`, `:X.Y` and `:latest` (no leading `v`).

- **SFTP file browser**: right-click an SSH connection → **Open file browser (SFTP)**. A tab lists the server's folders; double-click to go in, then upload files or whole folders, download selections to a folder, make folders, rename and delete. It logs in on its own (jump hosts, host-key and password prompts work as in a terminal).

- **Button bar**: save commands you type often and send each with one click. Turn the bar on with the terminal button in the toolbar, then use the pencil to add, reorder or remove buttons. A button can show for every connection or only for one folder (and what is inside it). Buttons sync to your other computers, and while broadcasting they go to every session in the group.

- **Broadcast input**: the radio button in the toolbar sends what you type to every open terminal tab at once. A red banner and a red count show it is on; the radio icon on each tab includes or leaves it. Tabs opened later join only if you include them. Remote screens (RDP/VNC) never take part.

- **Settings → Debug log**: Linecard keeps a small log of sync, SSH/Telnet/RDP/VNC and update errors (no terminal output, passwords or keys). Copy it or show its folder to attach to a bug report.

- Right-click a connection → **Saved password…** to save or replace its password without logging in first (SSH, RDP and VNC).
- VNC: the local clipboard is sent to the server when the screen gets focus (if it changed), so Ctrl+V on the remote side pastes it. Latin-1 text only, as the protocol allows.
- VNC and RDP connections can go through an SSH jump host (including chains and jump hosts inherited from folders). The jump host's host-key and password prompts appear in the tab, and RDP's login retries reuse the one jump-host login.

## v0.3.2 — tab shortcuts and close prompts, Windows 7 RDP

**Tabs**
- Middle-click a tab to close it.
- Closing a tab whose session is open asks first (with "Don't ask again"); quitting with connections open asks once for all of them. Both can be turned off in Settings → Tabs.
- Ctrl+Tab and Ctrl+Shift+Tab move to the next and previous tab, across panes.

**Connections pane**
- While dragging a connection or folder, the folder it would land in is highlighted (the whole list for the top level).

**Fixes**
- RDP to Windows 7 and Server 2008 R2: on Windows, Linecard now uses the system's TLS (as the built-in Remote Desktop client does), which still speaks TLS 1.0 to hosts that offer nothing newer. Before, those hosts dropped the connection ("forcibly closed by the remote host").
- RDP errors say which step failed (negotiation, TLS handshake or login) and what a dropped connection usually means.

## v0.3.1 — movable connections pane, update prompt

**Connections pane**
- Pin it open or set it to auto-hide (the pin in its title bar): hidden, it folds to a strip on the edge and slides out when you point at it or click it, then tucks away again.
- Drag its title bar to dock it on the left, right, top or bottom of the window. Its size is remembered for each orientation.
- Folders stay collapsed or expanded the way you left them after a restart.

**Updates**
- Linecard no longer downloads updates on its own: the status bar says when one is available, and clicking it shows what's new with a Download and install button.
- The version in the status bar opens that version's release notes.
- Each release on GitHub lists its changes.

## v0.3.0 — RDP, VNC, more ways to sync, password sync

**Graphical sessions**
- RDP: connections of type RDP open in a tab (IronRDP, on every platform). Network Level Authentication (CredSSP/NTLM) with the user name inherited like SSH and "Save password"; the server certificate is checked like an SSH host key before any password is sent; the remote desktop follows the tab's size when the server supports it.
- VNC: connections of type VNC open in a tab showing the remote screen, scaled to fit, with mouse, wheel and keyboard (Ctrl+Shift+V types the clipboard). VNC password authentication with "Save password"; ZRLE, CopyRect and Raw encodings; servers that change size.

**Sync**
- Sync saved passwords (opt-in, Settings → Saved passwords): encrypted with a vault passphrase that is separate from the sync phrase (Argon2id, then XChaCha20-Poly1305 per password). Each computer asks for the passphrase once; saving, changing or removing a password on one computer does the same on the others.
- Sync on the local network with no server: pick "This network" in Settings → Sync. Computers find each other by multicast and sync directly; each keeps and passes on what it has, so changes reach a computer that was off when they were made.
- Sync through a GitHub repository: pick GitHub in Settings → Sync, enter owner/name and a fine-grained token for that repository (Contents: read and write; kept in the keychain). Same layout as a shared folder, one commit per batch, checked every 30 seconds; the repository only holds ciphertext.
- Relay sync is near-instant: computers wait on the relay, which answers as soon as a change arrives (about a quarter second end to end), instead of checking every 5 seconds.
- Private relays: with LINECARD_RELAY_OPEN=false only accounts can start sync groups. The operator creates them with `linecard-relay account add`, which prints an access key to enter in Settings → Sync. Admin commands also list and remove groups and accounts (see docs/relay.md).

**Links and command line**
- Settings → Links and command line: "Add linecard to PATH" (Windows: your user PATH; Linux: ~/.local/bin; macOS shows the one-line command, which needs admin rights).
- ssh:// and telnet:// links can open in Linecard on macOS and Linux too (Launch Services / xdg-mime), and the handler they replace comes back when turned off.

**Fixes**
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
