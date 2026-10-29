//! Tauri shell: exposes the Rust core to the UI as commands and events.
//!
//! Phase 1 status: the tree lives in memory (persistence via core-store is
//! next); SSH and Telnet sessions are real; Serial is next (see
//! docs/roadmap.md).

mod dto;
mod highlight;
mod import;
mod launch;
mod library;
mod logging;
mod sync;
mod system;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use library::{now_ms, Library};
use linecard_core_model::{Connection, ConnectionKind, Endpoint, Folder, Settings, Tree};
use linecard_core_sync::Hlc;
use linecard_proto::{SerialSettings, Session, SshOptions, TerminalTarget};
use linecard_vault::{SavedCredential, Secret, Vault};
use logging::SessionLog;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager, State};

use dto::{ConnectionDraftDto, DeviceDto, PromptReplyDto, SessionEventDto, SyncStateDto, TreeNodeDto};

struct AppState {
    /// Connections and folders, persisted as an op log (see library.rs).
    lib: Mutex<Library>,
    sessions: Mutex<HashMap<String, Box<dyn Session>>>,
    ssh: SshOptions,
    vault: Arc<Vault>,
    /// Where session logs go unless a folder is configured.
    log_dir: std::path::PathBuf,
    sync: sync::SyncEngine,
}

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Op ids travel to the UI as "wall.counter@device".
pub(crate) fn encode_op_id(h: &Hlc) -> String {
    format!("{}.{}@{}", h.wall_ms, h.counter, h.device)
}

fn decode_op_id(s: &str) -> Option<Hlc> {
    let (stamp, device) = s.split_once('@')?;
    let (wall, counter) = stamp.split_once('.')?;
    Some(Hlc {
        wall_ms: wall.parse().ok()?,
        counter: counter.parse().ok()?,
        device: device.into(),
    })
}

fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn notify_tree_changed(app: &AppHandle) {
    let _ = app.emit("tree-changed", ());
}

/// First-run content so the app isn't empty.
fn seed_starter(tree: &mut Tree) {
    tree.global.keepalive_secs = Some(30);
    let folder = new_id();
    tree.add_folder(Folder {
        id: folder.clone(),
        name: "Examples".into(),
        parent: None,
        order: 0.0,
        color: None,
        defaults: Settings::default(),
    })
    .expect("starter folder");
    tree.add_connection(Connection {
        id: new_id(),
        name: "Telehack (public telnet)".into(),
        folder: Some(folder),
        order: 0.0,
        kind: ConnectionKind::Telnet,
        endpoint: Endpoint::Network {
            host: "telehack.com".into(),
            port: 23,
        },
        overrides: Settings::default(),
        notes: "A public telnet server, handy for checking that sessions work.".into(),
        tags: vec![],
        forwards: vec![],
    })
    .expect("starter connection");
    highlight::seed(tree);
}

/// Run a tree edit through the library (which records it as ops) and tell
/// the UI to refresh.
fn edit<T>(app: &AppHandle, state: &AppState, f: impl FnOnce(&mut Tree) -> CmdResult<T>) -> CmdResult<T> {
    let out = state.lib.lock().map_err(err)?.edit(f)?;
    notify_tree_changed(app);
    state.sync.poke();
    Ok(out)
}

// ---- tree --------------------------------------------------------------------

#[tauri::command]
fn get_tree(state: State<AppState>) -> CmdResult<Vec<TreeNodeDto>> {
    let lib = state.lib.lock().map_err(err)?;
    Ok(dto::tree_dto(lib.tree(), lib.changes(), None))
}

#[tauri::command]
fn move_node(
    app: AppHandle,
    state: State<AppState>,
    id: String,
    parent_id: Option<String>,
    index: usize,
) -> CmdResult<()> {
    edit(&app, &state, |tree| {
        let parent = parent_id.as_deref();
        let is_folder = tree.folder(&id).is_ok();
        // The UI's index still counts the dragged item, so moving it down
        // within its own folder lands one place too far without this.
        let (old_parent, old_pos) = if is_folder {
            let p = tree.folder(&id).map_err(err)?.parent.clone();
            let pos = tree.child_folders(p.as_deref()).iter().position(|f| f.id == id);
            (p, pos)
        } else {
            let p = tree.connection(&id).map_err(err)?.folder.clone();
            let offset = tree.child_folders(p.as_deref()).len();
            let pos = tree
                .child_connections(p.as_deref())
                .iter()
                .position(|c| c.id == id)
                .map(|i| i + offset);
            (p, pos)
        };
        let index = match old_pos {
            Some(old) if old_parent.as_deref() == parent && old < index => index - 1,
            _ => index,
        };
        if is_folder {
            tree.move_folder(&id, parent).map_err(err)?;
        } else {
            tree.move_connection(&id, parent).map_err(err)?;
        }
        // Renumber siblings of the same type so `index` is respected.
        let mut siblings: Vec<String> = if is_folder {
            tree.child_folders(parent).iter().map(|f| f.id.clone()).collect()
        } else {
            tree.child_connections(parent).iter().map(|c| c.id.clone()).collect()
        };
        siblings.retain(|s| s != &id);
        // The UI index counts folders before connections.
        let offset = if is_folder { 0 } else { tree.child_folders(parent).len() };
        siblings.insert(index.saturating_sub(offset).min(siblings.len()), id.clone());
        for (i, sid) in siblings.iter().enumerate() {
            if is_folder {
                tree.folder_mut(sid).map_err(err)?.order = i as f64;
            } else {
                tree.connection_mut(sid).map_err(err)?.order = i as f64;
            }
        }
        Ok(())
    })
}

/// Sort key that places a new item after its siblings.
fn next_order(orders: impl Iterator<Item = f64>) -> f64 {
    orders.fold(-1.0, f64::max) + 1.0
}

#[tauri::command]
fn create_folder(app: AppHandle, state: State<AppState>, parent_id: Option<String>, name: String) -> CmdResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Folder name is required".into());
    }
    edit(&app, &state, |tree| {
        let order = next_order(tree.child_folders(parent_id.as_deref()).iter().map(|f| f.order));
        let id = new_id();
        tree.add_folder(Folder {
            id: id.clone(),
            name: name.into(),
            parent: parent_id,
            order,
            color: None,
            defaults: Settings::default(),
        })
        .map_err(err)?;
        Ok(id)
    })
}

#[tauri::command]
fn get_connection(state: State<AppState>, id: String) -> CmdResult<ConnectionDraftDto> {
    let lib = state.lib.lock().map_err(err)?;
    Ok(lib.tree().connection(&id).map_err(err)?.into())
}

/// What a connection in `folder_id` gets when it doesn't set a value itself,
/// and where each value comes from ("/DC1/Spine" or "Settings").
#[tauri::command]
fn inherited_settings(state: State<AppState>, folder_id: Option<String>) -> CmdResult<dto::InheritedDto> {
    let lib = state.lib.lock().map_err(err)?;
    let mut tree = lib.tree().clone();
    // Resolve a throwaway connection placed in that folder, with no overrides.
    let probe = "__inherited_probe__".to_string();
    tree.add_connection(Connection {
        id: probe.clone(),
        name: String::new(),
        folder: folder_id.filter(|f| tree.folder(f).is_ok()),
        order: 0.0,
        kind: ConnectionKind::Ssh,
        endpoint: Endpoint::network(ConnectionKind::Ssh, ""),
        overrides: Settings::default(),
        notes: String::new(),
        tags: vec![],
        forwards: vec![],
    })
    .map_err(err)?;
    let r = tree.resolve(&probe).map_err(err)?;
    let from = |field: &str| match r.source_of(field) {
        Some(linecard_core_model::Source::Folder(id)) => tree.path(Some(id)).unwrap_or_default(),
        _ => "Settings".to_string(),
    };
    let item = |field: &str, value: Option<String>| {
        value.map(|value| dto::InheritedValue {
            value,
            from: from(field),
        })
    };
    Ok(dto::InheritedDto {
        username: item("username", r.settings.username.clone()),
        tab_color: item("tab_color", r.settings.tab_color.clone()),
        jump_host: item(
            "jump_host",
            r.settings
                .jump_host
                .as_deref()
                .and_then(|id| tree.connection(id).ok())
                .map(|c| c.name.clone()),
        ),
        log: item(
            "log_enabled",
            r.settings.log_enabled.map(|b| if b { "on" } else { "off" }.to_string()),
        ),
        highlight: item(
            "highlight_rules",
            r.settings
                .highlight_rules
                .as_deref()
                .and_then(|id| tree.highlight_profiles.get(id))
                .map(|p| p.name.clone()),
        )
        .or_else(|| {
            let id = highlight::default_id(&tree)?;
            Some(dto::InheritedValue {
                value: tree.highlight_profiles.get(&id)?.name.clone(),
                from: "Settings".into(),
            })
        }),
    })
}

/// A folder's own defaults (what its folders and connections inherit).
#[tauri::command]
fn get_folder_settings(state: State<AppState>, id: String) -> CmdResult<dto::FolderSettingsDto> {
    let lib = state.lib.lock().map_err(err)?;
    let f = lib.tree().folder(&id).map_err(err)?;
    Ok(dto::FolderSettingsDto::from(&f.defaults))
}

#[tauri::command]
fn save_folder_settings(
    app: AppHandle,
    state: State<AppState>,
    id: String,
    settings: dto::FolderSettingsDto,
) -> CmdResult<()> {
    edit(&app, &state, |tree| {
        let f = tree.folder_mut(&id).map_err(err)?;
        settings.apply_to(&mut f.defaults);
        Ok(())
    })
}

/// Create (`draft.id` is None) or update a connection. Returns its id.
#[tauri::command]
fn save_connection(app: AppHandle, state: State<AppState>, draft: ConnectionDraftDto) -> CmdResult<String> {
    edit(&app, &state, |tree| match &draft.id {
        Some(id) => {
            draft.apply_to(tree.connection_mut(id).map_err(err)?)?;
            Ok(id.clone())
        }
        None => {
            let folder = draft.folder_id.as_deref();
            let mut conn = Connection {
                id: new_id(),
                name: String::new(),
                folder: draft.folder_id.clone(),
                order: next_order(tree.child_connections(folder).iter().map(|c| c.order)),
                kind: draft.kind,
                endpoint: Endpoint::network(draft.kind, ""),
                overrides: Settings::default(),
                notes: String::new(),
                tags: vec![],
                forwards: vec![],
            };
            draft.apply_to(&mut conn)?;
            let id = conn.id.clone();
            tree.add_connection(conn).map_err(err)?;
            Ok(id)
        }
    })
}

#[tauri::command]
fn rename_node(app: AppHandle, state: State<AppState>, id: String, name: String) -> CmdResult<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Name is required".into());
    }
    edit(&app, &state, |tree| {
        match tree.folder_mut(&id) {
            Ok(f) => f.name = name.into(),
            Err(_) => tree.connection_mut(&id).map_err(err)?.name = name.into(),
        }
        Ok(())
    })
}

/// Delete a connection, or a folder with everything in it.
#[tauri::command]
fn delete_node(app: AppHandle, state: State<AppState>, id: String) -> CmdResult<()> {
    edit(&app, &state, |tree| {
        if tree.folder(&id).is_ok() {
            tree.remove_folder(&id).map_err(err)?;
        } else {
            tree.remove_connection(&id).map_err(err)?;
        }
        Ok(())
    })
}

/// Copy a connection, placed right after the original. Returns the copy's id.
#[tauri::command]
fn duplicate_connection(app: AppHandle, state: State<AppState>, id: String) -> CmdResult<String> {
    edit(&app, &state, |tree| {
        let mut copy = tree.connection(&id).map_err(err)?.clone();
        copy.id = new_id();
        copy.name = format!("{} (copy)", copy.name);
        copy.order += 0.5;
        let copy_id = copy.id.clone();
        tree.add_connection(copy).map_err(err)?;
        Ok(copy_id)
    })
}

// ---- change tracking -------------------------------------------------------

#[tauri::command]
fn ack_change(app: AppHandle, state: State<AppState>, object_id: String) -> CmdResult<()> {
    state.lib.lock().map_err(err)?.changes_mut().ack(&object_id);
    notify_tree_changed(&app);
    Ok(())
}

#[tauri::command]
fn ack_all(app: AppHandle, state: State<AppState>) -> CmdResult<()> {
    state.lib.lock().map_err(err)?.changes_mut().ack_all();
    notify_tree_changed(&app);
    Ok(())
}

/// Undo one change with a new op (which syncs, and can itself be reverted).
#[tauri::command]
fn revert_op(app: AppHandle, state: State<AppState>, op_id: String) -> CmdResult<()> {
    let id = decode_op_id(&op_id).ok_or("bad op id")?;
    state.lib.lock().map_err(err)?.revert(&id)?;
    notify_tree_changed(&app);
    Ok(())
}

// ---- preferences -------------------------------------------------------------

const PREFERENCES_KEY: &str = "preferences";

/// This device's UI preferences as the UI stored them (defaults live in the
/// UI), or null on first run.
#[tauri::command]
fn get_preferences(state: State<AppState>) -> CmdResult<Option<serde_json::Value>> {
    let raw = state.lib.lock().map_err(err)?.local_setting(PREFERENCES_KEY)?;
    Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
}

#[tauri::command]
fn set_preferences(state: State<AppState>, preferences: serde_json::Value) -> CmdResult<()> {
    state
        .lib
        .lock()
        .map_err(err)?
        .set_local_setting(PREFERENCES_KEY, &preferences.to_string())
}

// ---- import ----------------------------------------------------------------

#[tauri::command]
fn import_sources() -> Vec<import::ImportSourceDto> {
    import::sources()
}

/// What an import would add, without changing anything.
#[tauri::command]
async fn import_preview(source: String, path: Option<String>) -> CmdResult<import::ImportPreviewDto> {
    tauri::async_runtime::spawn_blocking(move || import::load(&source, path.as_deref()).map(|i| import::preview(&i)))
        .await
        .map_err(err)?
}

/// Import into a new top-level folder; returns its id.
#[tauri::command]
fn import_apply(app: AppHandle, state: State<AppState>, source: String, path: Option<String>) -> CmdResult<String> {
    let data = import::load(&source, path.as_deref())?;
    edit(&app, &state, |tree| import::apply(tree, &data, new_id))
}

// ---- session logging defaults ----------------------------------------------------

/// The app-wide logging defaults (folders and connections can override them).
#[tauri::command]
fn get_logging(state: State<AppState>) -> CmdResult<dto::LoggingDto> {
    let lib = state.lib.lock().map_err(err)?;
    let g = &lib.tree().global;
    Ok(dto::LoggingDto {
        enabled: g.log_enabled == Some(true),
        folder: g.log_path_template.clone(),
        timestamps: g.log_timestamps == Some(true),
        default_folder: Some(state.log_dir.display().to_string()),
    })
}

#[tauri::command]
fn set_logging(app: AppHandle, state: State<AppState>, logging: dto::LoggingDto) -> CmdResult<()> {
    edit(&app, &state, |tree| {
        tree.global.log_enabled = Some(logging.enabled);
        tree.global.log_timestamps = Some(logging.timestamps);
        tree.global.log_path_template = logging.folder.map(|f| f.trim().to_string()).filter(|f| !f.is_empty());
        Ok(())
    })
}

// ---- saved passwords ---------------------------------------------------------

/// What's saved in the keychain (labels only, never secrets).
#[tauri::command]
fn list_saved_passwords(state: State<AppState>) -> Vec<SavedCredential> {
    state.vault.list()
}

#[tauri::command]
fn forget_saved_password(state: State<AppState>, key: String) -> CmdResult<()> {
    state.vault.forget(&key).map_err(err)
}

// ---- devices & sync ----------------------------------------------------------

#[tauri::command]
fn get_devices(app: AppHandle, state: State<AppState>) -> CmdResult<Vec<DeviceDto>> {
    let device = state.lib.lock().map_err(err)?.device().to_string();
    Ok(vec![DeviceDto {
        id: device,
        name: std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "This computer".into()),
        os: std::env::consts::OS.into(),
        app_version: app.package_info().version.to_string(),
        protocol_version: linecard_core_sync::SYNC_PROTOCOL_VERSION,
        last_seen: now_ms(),
        transport: sync::SyncEngine::config(&app).map(|c| c.transport).unwrap_or_default(),
        pending_ops: sync::unpushed(&app).unwrap_or(0) as u32,
        is_this_device: true,
    }]
    .into_iter()
    .chain(sync::SyncEngine::devices(&app))
    .collect())
}

#[tauri::command]
fn get_sync_state(state: State<AppState>) -> SyncStateDto {
    state.sync.status()
}

#[tauri::command]
fn get_sync_config(app: AppHandle) -> Option<sync::SyncConfig> {
    sync::SyncEngine::config(&app)
}

/// Create a sync group (no phrase) or join one; returns the phrase.
#[tauri::command]
async fn sync_setup(app: AppHandle, transport: String, location: String, phrase: Option<String>) -> CmdResult<String> {
    sync::SyncEngine::setup(&app, sync::SyncConfig { transport, location }, phrase).await
}

#[tauri::command]
fn sync_phrase(app: AppHandle) -> Option<String> {
    sync::SyncEngine::phrase(&app)
}

/// New phrase for the group (removing a computer, or a leaked phrase).
#[tauri::command]
async fn sync_rotate(app: AppHandle) -> CmdResult<String> {
    sync::SyncEngine::rotate(&app).await
}

#[tauri::command]
fn sync_now(state: State<AppState>) {
    state.sync.poke();
}

#[tauri::command]
fn sync_disable(app: AppHandle) -> CmdResult<()> {
    sync::SyncEngine::disable(&app)
}

// ---- sessions ----------------------------------------------------------------

/// Quick-connect endpoint: `[user@]host[:port]`, with `[v6addr]:port` for IPv6.
fn parse_endpoint(endpoint: &str, kind: ConnectionKind) -> Option<(Option<String>, String, u16)> {
    let (user, rest) = match endpoint.rsplit_once('@') {
        Some((u, r)) if !u.is_empty() => (Some(u.to_string()), r),
        _ => (None, endpoint),
    };
    let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
        let (h, tail) = v6.split_once(']')?;
        (h, tail.strip_prefix(':'))
    } else if rest.matches(':').count() == 1 {
        let (h, p) = rest.split_once(':')?;
        (h, Some(p))
    } else {
        (rest, None) // bare hostname, or a bare IPv6 address
    };
    if host.is_empty() {
        return None;
    }
    let port = match port {
        Some(p) => p.parse().ok()?,
        None => kind.default_port()?,
    };
    Some((user, host.to_string(), port))
}

/// DocumentsLinecard Logs (falls back to the app data folder).
fn default_log_dir(app: &AppHandle) -> std::path::PathBuf {
    app.path()
        .document_dir()
        .or_else(|_| app.path().app_data_dir())
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("Linecard Logs")
}

/// Saved passwords for the SSH driver, kept in the OS keychain.
struct VaultCredentials(Arc<Vault>);

impl std::fmt::Debug for VaultCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VaultCredentials")
    }
}

impl linecard_proto::Credentials for VaultCredentials {
    fn password(&self, key: &str) -> Option<String> {
        self.0.get(key).map(|s| s.expose().to_string())
    }
    fn save_password(&self, key: &str, label: &str, password: &str) {
        // A failed save only means the user is asked again next time.
        let _ = self.0.save(key, label, Secret::new(password));
    }
}

/// Where SSH keeps keys and trusted hosts. Linecard writes its own
/// known_hosts and only reads the user's OpenSSH one.
fn ssh_options(app: &AppHandle, vault: Arc<Vault>) -> SshOptions {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from);
    let dot_ssh = home.map(|h| h.join(".ssh"));
    let data = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("linecard"));
    SshOptions {
        credentials: Some(Arc::new(VaultCredentials(vault))),
        known_hosts: data.join("known_hosts"),
        extra_known_hosts: dot_ssh.iter().map(|d| d.join("known_hosts")).collect(),
        identity_files: dot_ssh
            .iter()
            .flat_map(|d| ["id_ed25519", "id_ecdsa", "id_rsa"].map(|f| d.join(f)))
            .collect(),
        use_agent: true,
    }
}

/// Quick-connect serial: "COM3", "COM3 115200", "/dev/ttyUSB0 9600" (8N1).
fn parse_serial(endpoint: &str) -> Option<SerialSettings> {
    let mut parts = endpoint.split_whitespace();
    let mut s = SerialSettings::console(parts.next()?);
    if let Some(baud) = parts.next() {
        s.baud = baud.parse().ok()?;
    }
    Some(s)
}

/// Ports present now, for the connection editor.
#[tauri::command]
fn list_serial_ports() -> Vec<dto::SerialPortDto> {
    linecard_proto::serial::list_ports()
        .into_iter()
        .map(|p| dto::SerialPortDto {
            name: p.name,
            description: p.description,
        })
        .collect()
}

/// Start an XMODEM/YMODEM transfer on a serial session.
#[tauri::command]
fn session_transfer(
    state: State<AppState>,
    session_id: String,
    send: bool,
    protocol: String,
    path: String,
) -> CmdResult<()> {
    use linecard_proto::xmodem::Protocol;
    let protocol = match protocol.as_str() {
        "xmodem" => Protocol::Xmodem,
        "xmodem1k" => Protocol::Xmodem1k,
        "ymodem" => Protocol::Ymodem,
        other => return Err(format!("unknown protocol {other}")),
    };
    let path = std::path::PathBuf::from(path);
    let req = if send {
        linecard_proto::TransferRequest::Send { path, protocol }
    } else {
        linecard_proto::TransferRequest::Receive { path, protocol }
    };
    let mut sessions = state.sessions.lock().map_err(err)?;
    let s = sessions.get_mut(&session_id).ok_or("session closed")?;
    s.start_transfer(req).map_err(err)
}

#[tauri::command]
fn session_cancel_transfer(state: State<AppState>, session_id: String) -> CmdResult<()> {
    if let Some(s) = state.sessions.lock().map_err(err)?.get_mut(&session_id) {
        s.cancel_transfer();
    }
    Ok(())
}

/// Send BREAK on a serial session.
#[tauri::command]
async fn session_break(state: State<'_, AppState>, session_id: String) -> CmdResult<()> {
    let mut sessions = state.sessions.lock().map_err(err)?;
    let s = sessions.get_mut(&session_id).ok_or("session closed")?;
    s.send_break().map_err(err)
}

/// Where a saved connection goes, with its inherited login settings and its
/// jump host (itself a saved SSH connection, resolved the same way).
fn network_target(tree: &Tree, id: &str, depth: usize) -> CmdResult<TerminalTarget> {
    if depth > 8 {
        return Err("jump hosts form a loop".into());
    }
    let c = tree.connection(id).map_err(err)?;
    let Endpoint::Network { host, port } = &c.endpoint else {
        return Err(format!("{} is a serial connection", c.name));
    };
    let settings = tree.resolve(&c.id).map_err(err)?.settings;
    let mut t = TerminalTarget::new(host.clone(), *port);
    t.username = settings.username;
    t.keepalive_secs = settings.keepalive_secs;
    // A shared credential (e.g. set on a folder) pools the saved password.
    t.credential_key = settings.credential_id.map(|id| format!("cred:{id}"));
    if let Some(jump) = settings.jump_host.filter(|j| j != id) {
        let hop = tree
            .connection(&jump)
            .map_err(|_| format!("the jump host for {} no longer exists", c.name))?;
        if hop.kind != ConnectionKind::Ssh {
            return Err(format!("jump host {} must be an SSH connection", hop.name));
        }
        t.jump = Some(Box::new(network_target(tree, &jump, depth + 1)?));
    }
    Ok(t)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn open_session(
    state: State<'_, AppState>,
    connection_id: String,
    kind: ConnectionKind,
    endpoint: String,
    cols: u16,
    rows: u16,
    on_event: Channel<SessionEventDto>,
) -> CmdResult<String> {
    enum Plan {
        Network(ConnectionKind, TerminalTarget),
        Serial(SerialSettings),
    }
    // Saved connections use their stored endpoint and inherited settings;
    // quick-connect uses what the UI sent.
    let (plan, settings, name) = {
        let lib = state.lib.lock().map_err(err)?;
        let tree = lib.tree();
        match tree.connection(&connection_id) {
            Ok(c) => {
                let settings = tree.resolve(&c.id).map_err(err)?.settings;
                let plan = match &c.endpoint {
                    Endpoint::Serial(s) => Plan::Serial(s.clone()),
                    Endpoint::Network { .. } => {
                        let mut t = network_target(tree, &c.id, 0)?;
                        // Port forwards belong to the connection opened, not its jump hosts.
                        t.forwards = c.forwards.clone();
                        Plan::Network(c.kind, t)
                    }
                };
                (plan, settings, c.name.clone())
            }
            Err(_) if kind == ConnectionKind::Serial => {
                let s = parse_serial(&endpoint).ok_or("expected a port and baud rate, e.g. COM3 9600")?;
                let name = s.port.clone();
                (Plan::Serial(s), tree.global.clone(), name)
            }
            Err(_) => {
                let (user, host, port) = parse_endpoint(&endpoint, kind).ok_or("could not read host and port")?;
                let mut t = TerminalTarget::new(host.clone(), port);
                t.username = user;
                t.keepalive_secs = tree.global.keepalive_secs;
                (Plan::Network(kind, t), tree.global.clone(), host)
            }
        }
    };

    // Session log, if this connection (or its folder, or the global default) asks for one.
    let mut log = None;
    if settings.log_enabled == Some(true) {
        let path = logging::log_path(
            settings.log_path_template.as_deref(),
            &state.log_dir,
            &name,
            &endpoint,
            chrono::Local::now(),
        );
        match SessionLog::open(path.clone(), settings.log_timestamps == Some(true)) {
            Ok(mut l) => {
                l.note(&format!("{name} ({endpoint}) session opened"));
                let _ = on_event.send(SessionEventDto::Data {
                    data: format!("\x1b[2mLogging to {}\x1b[0m\r\n", path.display()).into_bytes(),
                });
                log = Some(l);
            }
            Err(e) => {
                let _ = on_event.send(SessionEventDto::Data {
                    data: format!("\x1b[33mCould not start the session log: {e}\x1b[0m\r\n").into_bytes(),
                });
            }
        }
    }

    let ssh = state.ssh.clone();
    let sink: linecard_proto::EventSink = Box::new(move |e| {
        if let Some(l) = &mut log {
            match &e {
                linecard_proto::SessionEvent::Data(d) => l.write(d),
                linecard_proto::SessionEvent::Disconnected { reason } => l.note(&format!(
                    "session closed{}",
                    reason.as_ref().map(|r| format!(": {r}")).unwrap_or_default()
                )),
                _ => {}
            }
        }
        let _ = on_event.send(SessionEventDto::from(e));
    });
    // Telnet and serial connect before returning; SSH returns at once and
    // reports progress as events.
    let session = tauri::async_runtime::spawn_blocking(move || match plan {
        Plan::Serial(s) => linecard_proto::connect_serial(&s, sink),
        Plan::Network(kind, mut target) => {
            target.cols = cols;
            target.rows = rows;
            linecard_proto::connect_terminal(kind, &target, &ssh, sink)
        }
    })
    .await
    .map_err(err)?
    .map_err(err)?;

    let id = new_id();
    state.sessions.lock().map_err(err)?.insert(id.clone(), session);
    Ok(id)
}

#[tauri::command]
fn session_write(state: State<AppState>, session_id: String, data: Vec<u8>) -> CmdResult<()> {
    let mut sessions = state.sessions.lock().map_err(err)?;
    let s = sessions.get_mut(&session_id).ok_or("session closed")?;
    s.write(&data).map_err(err)
}

#[tauri::command]
fn session_resize(state: State<AppState>, session_id: String, cols: u16, rows: u16) -> CmdResult<()> {
    let mut sessions = state.sessions.lock().map_err(err)?;
    let s = sessions.get_mut(&session_id).ok_or("session closed")?;
    s.resize(cols, rows).map_err(err)
}

/// Answer a host-key or credential prompt.
#[tauri::command]
fn session_reply(state: State<AppState>, session_id: String, reply: PromptReplyDto) -> CmdResult<()> {
    let mut sessions = state.sessions.lock().map_err(err)?;
    let s = sessions.get_mut(&session_id).ok_or("session closed")?;
    s.reply(reply.into()).map_err(err)
}

#[tauri::command]
fn session_close(state: State<AppState>, session_id: String) -> CmdResult<()> {
    if let Some(mut s) = state.sessions.lock().map_err(err)?.remove(&session_id) {
        let _ = s.close();
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();
    // One window: a second launch (command line or link) opens its tab there.
    // A separate profile (LINECARD_DATA_DIR) runs as its own instance.
    if std::env::var_os("LINECARD_DATA_DIR").is_none() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            launch::second_instance(app, argv)
        }));
    }
    builder
        .manage(launch::Pending(std::sync::Mutex::new(
            std::env::args().skip(1).collect(),
        )))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // LINECARD_DATA_DIR runs a separate profile (e.g. a second
            // "computer" for trying sync on one machine).
            let data = match std::env::var_os("LINECARD_DATA_DIR") {
                Some(dir) => std::path::PathBuf::from(dir),
                None => app.path().app_data_dir()?,
            };
            let mut lib = Library::open(&data.join("library.db"), seed_starter)
                .map_err(|e| format!("could not open the connection library: {e}"))?;
            highlight::migrate(&mut lib, PREFERENCES_KEY)?;
            // Secrets live in the OS keychain; this file only lists what's saved.
            let vault = Arc::new(Vault::keychain(data.join("credentials.json")));
            app.manage(AppState {
                lib: Mutex::new(lib),
                sessions: Mutex::new(HashMap::new()),
                ssh: ssh_options(app.handle(), vault.clone()),
                vault,
                log_dir: default_log_dir(app.handle()),
                sync: sync::SyncEngine::default(),
            });
            sync::SyncEngine::start(app.handle());
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_title("Linecard");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_tree,
            move_node,
            create_folder,
            get_connection,
            inherited_settings,
            save_connection,
            rename_node,
            delete_node,
            duplicate_connection,
            ack_change,
            ack_all,
            revert_op,
            get_preferences,
            set_preferences,
            list_saved_passwords,
            get_logging,
            import_sources,
            import_preview,
            import_apply,
            set_logging,
            forget_saved_password,
            get_devices,
            get_sync_state,
            get_folder_settings,
            save_folder_settings,
            launch::take_launch,
            launch::link_handler_status,
            launch::set_link_handler,
            launch::cli_path_status,
            launch::set_cli_path,
            highlight::get_highlight,
            highlight::effective_highlight,
            highlight::save_highlight_profile,
            highlight::delete_highlight_profile,
            highlight::set_default_highlight,
            highlight::securecrt_keyword_files,
            highlight::import_highlight,
            get_sync_config,
            sync_setup,
            sync_phrase,
            sync_now,
            sync_rotate,
            sync_disable,
            open_session,
            session_write,
            session_resize,
            session_reply,
            session_break,
            session_transfer,
            session_cancel_transfer,
            list_serial_ports,
            session_close,
        ])
        .build(tauri::generate_context!())
        .expect("error while starting Linecard")
        .run(|_app, _event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Opened { urls } = _event {
                launch::opened_urls(_app, urls.into_iter().map(|u| u.to_string()).collect());
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_ids_round_trip() {
        let h = Hlc {
            wall_ms: 1_730_000_000_000,
            counter: 3,
            device: "0192f3a1-7c2e-7b10-9d4e-1a2b3c4d5e6f".into(),
        };
        assert_eq!(decode_op_id(&encode_op_id(&h)), Some(h));
        assert_eq!(decode_op_id("garbage"), None);
    }

    #[test]
    fn endpoint_parsing() {
        let p = |s| parse_endpoint(s, ConnectionKind::Ssh);
        assert_eq!(p("sw1:2222"), Some((None, "sw1".into(), 2222)));
        assert_eq!(p("admin@10.0.0.1"), Some((Some("admin".into()), "10.0.0.1".into(), 22)));
        assert_eq!(p("admin@sw1:2222"), Some((Some("admin".into()), "sw1".into(), 2222)));
        assert_eq!(p("[fe80::1]:2222"), Some((None, "fe80::1".into(), 2222)));
        assert_eq!(
            p("fe80::1"),
            Some((None, "fe80::1".into(), 22)),
            "bare IPv6, default port"
        );
        assert_eq!(p("sw1:notaport"), None);
        assert_eq!(p(""), None);
        assert_eq!(
            parse_endpoint("cs1", ConnectionKind::Telnet),
            Some((None, "cs1".into(), 23))
        );
    }

    fn draft(kind: ConnectionKind) -> ConnectionDraftDto {
        ConnectionDraftDto {
            id: None,
            folder_id: None,
            name: " core1 ".into(),
            kind,
            host: " 10.0.0.1 ".into(),
            port: 22,
            serial: None,
            username: Some("  ".into()),
            tab_color: Some("#d64545".into()),
            jump_host: None,
            log: None,
            highlight: None,
            forwards: vec![],
            notes: String::new(),
        }
    }

    #[test]
    fn drafts_validate_and_round_trip() {
        let mut tree = Tree::default();
        seed_starter(&mut tree);
        let mut conn = tree.connections().next().unwrap().clone();

        draft(ConnectionKind::Ssh).apply_to(&mut conn).unwrap();
        assert_eq!(conn.name, "core1");
        assert_eq!(conn.endpoint.label(), "10.0.0.1:22");
        assert_eq!(conn.overrides.username, None, "blank username inherits");
        let back = ConnectionDraftDto::from(&conn);
        assert_eq!(back.tab_color.as_deref(), Some("#d64545"));

        let mut bad = draft(ConnectionKind::Ssh);
        bad.host = " ".into();
        assert!(bad.apply_to(&mut conn).is_err());
        let mut bad = draft(ConnectionKind::Telnet);
        bad.port = 0;
        assert!(bad.apply_to(&mut conn).is_err());
        assert!(
            draft(ConnectionKind::Serial).apply_to(&mut conn).is_err(),
            "serial needs settings"
        );
    }

    #[test]
    fn jump_hosts_resolve_through_folders_and_reject_loops() {
        let ssh = |id: &str, folder: Option<&str>, jump: Option<&str>| Connection {
            id: id.into(),
            name: id.into(),
            folder: folder.map(Into::into),
            order: 0.0,
            kind: ConnectionKind::Ssh,
            endpoint: Endpoint::network(ConnectionKind::Ssh, format!("{id}.lab")),
            overrides: Settings {
                jump_host: jump.map(Into::into),
                ..Default::default()
            },
            notes: String::new(),
            tags: vec![],
            forwards: vec![],
        };
        let mut tree = Tree::default();
        tree.add_folder(Folder {
            id: "dc".into(),
            name: "DC".into(),
            parent: None,
            order: 0.0,
            color: None,
            defaults: Settings {
                jump_host: Some("bastion".into()),
                ..Default::default()
            },
        })
        .unwrap();
        tree.add_connection(ssh("bastion", Some("dc"), None)).unwrap();
        tree.add_connection(ssh("core1", Some("dc"), None)).unwrap();

        let t = network_target(&tree, "core1", 0).unwrap();
        assert_eq!(t.host, "core1.lab");
        let hop = t.jump.expect("inherits the folder's jump host");
        assert_eq!(hop.host, "bastion.lab");
        assert!(hop.jump.is_none(), "the bastion doesn't jump through itself");

        tree.add_connection(ssh("a", None, Some("b"))).unwrap();
        tree.add_connection(ssh("b", None, Some("a"))).unwrap();
        assert_eq!(network_target(&tree, "a", 0).unwrap_err(), "jump hosts form a loop");
    }

    #[test]
    fn serial_quick_connect() {
        let s = parse_serial("COM3 115200").unwrap();
        assert_eq!((s.port.as_str(), s.baud, s.data_bits), ("COM3", 115200, 8));
        assert_eq!(parse_serial("COM4").unwrap().baud, 9600);
        assert_eq!(parse_serial("COM3 fast"), None);
        assert_eq!(parse_serial(""), None);
    }

    #[test]
    fn next_order_appends() {
        assert_eq!(next_order([].into_iter()), 0.0);
        assert_eq!(next_order([0.0, 3.0, 1.5].into_iter()), 4.0);
    }
}
