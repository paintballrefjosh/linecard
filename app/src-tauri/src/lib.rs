//! Tauri shell: exposes the Rust core to the UI as commands and events.
//!
//! Phase 1 status: the tree lives in memory (persistence via core-store is
//! next); SSH and Telnet sessions are real; Serial is next (see
//! docs/roadmap.md).

mod dto;

use std::collections::HashMap;
use std::sync::Mutex;

use linecard_core_model::{Connection, ConnectionKind, Endpoint, Folder, Settings, Tree};
use linecard_core_sync::{Hlc, Replica};
use linecard_proto::{Session, SshOptions, TerminalTarget};
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager, State};

use dto::{ConnectionDraftDto, DeviceDto, PromptReplyDto, SessionEventDto, SyncStateDto, TreeNodeDto};

struct AppState {
    device: String,
    tree: Mutex<Tree>,
    replica: Mutex<Replica>,
    sessions: Mutex<HashMap<String, Box<dyn Session>>>,
    ssh: SshOptions,
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn notify_tree_changed(app: &AppHandle) {
    let _ = app.emit("tree-changed", ());
}

/// First-run content so the app isn't empty.
fn starter_tree() -> Tree {
    let mut tree = Tree::new(Settings {
        keepalive_secs: Some(30),
        ..Default::default()
    });
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
    })
    .expect("starter connection");
    tree
}

// ---- tree --------------------------------------------------------------------

#[tauri::command]
fn get_tree(state: State<AppState>) -> CmdResult<Vec<TreeNodeDto>> {
    let tree = state.tree.lock().map_err(err)?;
    let replica = state.replica.lock().map_err(err)?;
    Ok(dto::tree_dto(&tree, &replica.changes, None))
}

#[tauri::command]
fn move_node(
    app: AppHandle,
    state: State<AppState>,
    id: String,
    parent_id: Option<String>,
    index: usize,
) -> CmdResult<()> {
    let mut tree = state.tree.lock().map_err(err)?;
    let parent = parent_id.as_deref();
    let is_folder = tree.folder(&id).is_ok();
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
    siblings.insert(index.saturating_sub(offset).min(siblings.len()), id);
    for (i, sid) in siblings.iter().enumerate() {
        if is_folder {
            tree.folder_mut(sid).map_err(err)?.order = i as f64;
        } else {
            tree.connection_mut(sid).map_err(err)?.order = i as f64;
        }
    }
    drop(tree);
    notify_tree_changed(&app);
    Ok(())
}

// Phase 2: these edit the tree directly; once the replica is the source of
// truth each becomes an op (create / update / delete) that syncs.

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
    let mut tree = state.tree.lock().map_err(err)?;
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
    drop(tree);
    notify_tree_changed(&app);
    Ok(id)
}

#[tauri::command]
fn get_connection(state: State<AppState>, id: String) -> CmdResult<ConnectionDraftDto> {
    let tree = state.tree.lock().map_err(err)?;
    Ok(tree.connection(&id).map_err(err)?.into())
}

/// Create (`draft.id` is None) or update a connection. Returns its id.
#[tauri::command]
fn save_connection(app: AppHandle, state: State<AppState>, draft: ConnectionDraftDto) -> CmdResult<String> {
    let mut tree = state.tree.lock().map_err(err)?;
    let id = match &draft.id {
        Some(id) => {
            draft.apply_to(tree.connection_mut(id).map_err(err)?)?;
            id.clone()
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
            };
            draft.apply_to(&mut conn)?;
            let id = conn.id.clone();
            tree.add_connection(conn).map_err(err)?;
            id
        }
    };
    drop(tree);
    notify_tree_changed(&app);
    Ok(id)
}

#[tauri::command]
fn rename_node(app: AppHandle, state: State<AppState>, id: String, name: String) -> CmdResult<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Name is required".into());
    }
    let mut tree = state.tree.lock().map_err(err)?;
    match tree.folder_mut(&id) {
        Ok(f) => f.name = name.into(),
        Err(_) => tree.connection_mut(&id).map_err(err)?.name = name.into(),
    }
    drop(tree);
    notify_tree_changed(&app);
    Ok(())
}

/// Delete a connection, or a folder with everything in it.
#[tauri::command]
fn delete_node(app: AppHandle, state: State<AppState>, id: String) -> CmdResult<()> {
    let mut tree = state.tree.lock().map_err(err)?;
    if tree.folder(&id).is_ok() {
        tree.remove_folder(&id).map_err(err)?;
    } else {
        tree.remove_connection(&id).map_err(err)?;
    }
    drop(tree);
    notify_tree_changed(&app);
    Ok(())
}

/// Copy a connection, placed right after the original. Returns the copy's id.
#[tauri::command]
fn duplicate_connection(app: AppHandle, state: State<AppState>, id: String) -> CmdResult<String> {
    let mut tree = state.tree.lock().map_err(err)?;
    let mut copy = tree.connection(&id).map_err(err)?.clone();
    copy.id = new_id();
    copy.name = format!("{} (copy)", copy.name);
    copy.order += 0.5;
    let copy_id = copy.id.clone();
    tree.add_connection(copy).map_err(err)?;
    drop(tree);
    notify_tree_changed(&app);
    Ok(copy_id)
}

// ---- change tracking -------------------------------------------------------

#[tauri::command]
fn ack_change(app: AppHandle, state: State<AppState>, object_id: String) -> CmdResult<()> {
    state.replica.lock().map_err(err)?.changes.ack(&object_id);
    notify_tree_changed(&app);
    Ok(())
}

#[tauri::command]
fn ack_all(app: AppHandle, state: State<AppState>) -> CmdResult<()> {
    state.replica.lock().map_err(err)?.changes.ack_all();
    notify_tree_changed(&app);
    Ok(())
}

#[tauri::command]
fn revert_op(app: AppHandle, state: State<AppState>, op_id: String) -> CmdResult<()> {
    let id = decode_op_id(&op_id).ok_or("bad op id")?;
    let mut replica = state.replica.lock().map_err(err)?;
    let _op = replica.revert(&id, now_ms()).map_err(err)?;
    // Phase 2: push `_op` to the sync transport and project the replica onto the tree.
    drop(replica);
    notify_tree_changed(&app);
    Ok(())
}

// ---- devices & sync ----------------------------------------------------------

#[tauri::command]
fn get_devices(app: AppHandle, state: State<AppState>) -> Vec<DeviceDto> {
    vec![DeviceDto {
        id: state.device.clone(),
        name: std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "This computer".into()),
        os: std::env::consts::OS.into(),
        app_version: app.package_info().version.to_string(),
        protocol_version: linecard_core_sync::SYNC_PROTOCOL_VERSION,
        last_seen: now_ms(),
        transport: "relay".into(),
        pending_ops: 0,
        is_this_device: true,
    }]
}

#[tauri::command]
fn get_sync_state() -> SyncStateDto {
    SyncStateDto::Off
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

/// Where SSH keeps keys and trusted hosts. Linecard writes its own
/// known_hosts and only reads the user's OpenSSH one.
fn ssh_options(app: &AppHandle) -> SshOptions {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from);
    let dot_ssh = home.map(|h| h.join(".ssh"));
    let data = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("linecard"));
    SshOptions {
        known_hosts: data.join("known_hosts"),
        extra_known_hosts: dot_ssh.iter().map(|d| d.join("known_hosts")).collect(),
        identity_files: dot_ssh
            .iter()
            .flat_map(|d| ["id_ed25519", "id_ecdsa", "id_rsa"].map(|f| d.join(f)))
            .collect(),
        use_agent: true,
    }
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
    // Saved connections use their stored endpoint and inherited settings;
    // quick-connect uses what the UI sent.
    let target = {
        let tree = state.tree.lock().map_err(err)?;
        match tree.connection(&connection_id) {
            Ok(c) => {
                let Endpoint::Network { host, port } = &c.endpoint else {
                    return Err("Serial sessions arrive in the next Phase 1 milestone".into());
                };
                let settings = tree.resolve(&c.id).map_err(err)?.settings;
                let mut t = TerminalTarget::new(host.clone(), *port);
                t.username = settings.username;
                t.keepalive_secs = settings.keepalive_secs;
                (c.kind, t)
            }
            Err(_) => {
                let (user, host, port) = parse_endpoint(&endpoint, kind).ok_or("could not read host and port")?;
                let mut t = TerminalTarget::new(host, port);
                t.username = user;
                t.keepalive_secs = tree.global.keepalive_secs;
                (kind, t)
            }
        }
    };
    let (kind, mut target) = target;
    target.cols = cols;
    target.rows = rows;
    let ssh = state.ssh.clone();
    let sink: linecard_proto::EventSink = Box::new(move |e| {
        let _ = on_event.send(SessionEventDto::from(e));
    });
    // Telnet connects before returning; SSH returns at once and reports progress as events.
    let session =
        tauri::async_runtime::spawn_blocking(move || linecard_proto::connect_terminal(kind, &target, &ssh, sink))
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
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let device = new_id();
            app.manage(AppState {
                replica: Mutex::new(Replica::new(device.clone())),
                device,
                tree: Mutex::new(starter_tree()),
                sessions: Mutex::new(HashMap::new()),
                ssh: ssh_options(app.handle()),
            });
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
            save_connection,
            rename_node,
            delete_node,
            duplicate_connection,
            ack_change,
            ack_all,
            revert_op,
            get_devices,
            get_sync_state,
            open_session,
            session_write,
            session_resize,
            session_reply,
            session_close,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Linecard");
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
            notes: String::new(),
        }
    }

    #[test]
    fn drafts_validate_and_round_trip() {
        let tree = starter_tree();
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
    fn next_order_appends() {
        assert_eq!(next_order([].into_iter()), 0.0);
        assert_eq!(next_order([0.0, 3.0, 1.5].into_iter()), 4.0);
    }
}
