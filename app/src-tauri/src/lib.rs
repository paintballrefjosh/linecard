//! Tauri shell: exposes the Rust core to the UI as commands and events.
//!
//! Phase 1 status: the tree lives in memory (persistence via core-store and
//! sync arrive with Phase 2); Telnet sessions are real; SSH and Serial
//! drivers are next (see docs/roadmap.md).

mod dto;

use std::collections::HashMap;
use std::sync::Mutex;

use linecard_core_model::{Connection, ConnectionKind, Endpoint, Folder, Settings, Tree};
use linecard_core_sync::{Hlc, Replica};
use linecard_proto::{Session, TerminalTarget};
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager, State};

use dto::{DeviceDto, SessionEventDto, SyncStateDto, TreeNodeDto};

struct AppState {
    device: String,
    tree: Mutex<Tree>,
    replica: Mutex<Replica>,
    sessions: Mutex<HashMap<String, Box<dyn Session>>>,
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

fn parse_host_port(endpoint: &str, kind: ConnectionKind) -> Option<(String, u16)> {
    match endpoint.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() => Some((h.to_string(), p.parse().ok()?)),
        _ => Some((endpoint.to_string(), kind.default_port()?)),
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
    // Saved connections use their stored endpoint; quick-connect uses what the UI sent.
    let (kind, host, port) = {
        let tree = state.tree.lock().map_err(err)?;
        match tree.connection(&connection_id) {
            Ok(c) => match &c.endpoint {
                Endpoint::Network { host, port } => (c.kind, host.clone(), *port),
                Endpoint::Serial(_) => return Err("Serial sessions arrive in the next Phase 1 milestone".into()),
            },
            Err(_) => {
                let (h, p) = parse_host_port(&endpoint, kind).ok_or("could not read host and port")?;
                (kind, h, p)
            }
        }
    };

    let mut target = TerminalTarget::new(host, port);
    target.cols = cols;
    target.rows = rows;
    let sink: linecard_proto::EventSink = Box::new(move |e| {
        let _ = on_event.send(SessionEventDto::from(e));
    });
    let session = tauri::async_runtime::spawn_blocking(move || linecard_proto::connect_terminal(kind, &target, sink))
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

#[tauri::command]
fn session_close(state: State<AppState>, session_id: String) -> CmdResult<()> {
    if let Some(mut s) = state.sessions.lock().map_err(err)?.remove(&session_id) {
        let _ = s.close();
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let device = new_id();
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(AppState {
            replica: Mutex::new(Replica::new(device.clone())),
            device,
            tree: Mutex::new(starter_tree()),
            sessions: Mutex::new(HashMap::new()),
        })
        .setup(|app| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_title("Linecard");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_tree,
            move_node,
            ack_change,
            ack_all,
            revert_op,
            get_devices,
            get_sync_state,
            open_session,
            session_write,
            session_resize,
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
    fn host_port_parsing() {
        assert_eq!(
            parse_host_port("sw1:2222", ConnectionKind::Ssh),
            Some(("sw1".into(), 2222))
        );
        assert_eq!(parse_host_port("sw1", ConnectionKind::Telnet), Some(("sw1".into(), 23)));
    }
}
