//! The sync loop: push this device's new ops, pull everyone else's, publish
//! a heartbeat. Runs as a background task while sync is set up; local edits
//! wake it immediately.
//!
//! Secrets (the group key and this device's signing key) live in the OS
//! keychain; the transport choice, cursors and pinned peer keys are local
//! settings in the library database.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use linecard_core_sync::{Hlc, Op};
use linecard_sync::crypto::{open, seal};
use linecard_sync::folder::{FolderCursor, FolderTransport};
use linecard_sync::relay::RelayTransport;
use linecard_sync::{Batch, DeviceInfo, DeviceKey, GroupKey, Notice, Received, Sealed, SyncError};
use linecard_vault::{Secret, Vault};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::dto::{DeviceDto, SyncStateDto};
use crate::library::now_ms;
use crate::AppState;

const CONFIG_KEY: &str = "sync_config";
const PUSHED_KEY: &str = "sync_pushed";
const PEER_PREFIX: &str = "sync_peer:";
/// Set when another computer closed the group (why sync stopped).
const CLOSED_KEY: &str = "sync_closed";
const HEARTBEAT_EVERY: u64 = 60_000;
/// Ops per sealed batch (keeps blobs well under the relay's limit).
const BATCH_OPS: usize = 400;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncConfig {
    /// "folder" or "relay"
    pub transport: String,
    /// Folder path or relay URL.
    pub location: String,
}

impl SyncConfig {
    fn label(&self) -> String {
        match self.transport.as_str() {
            "folder" => "shared folder".into(),
            _ => "relay".into(),
        }
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(if self.transport == "relay" { 5 } else { 10 })
    }
}

enum Transport {
    Folder(Arc<FolderTransport>),
    Relay(RelayTransport),
}

fn blocking_err(e: tauri::Error) -> SyncError {
    SyncError::Storage(e.to_string())
}

impl Transport {
    fn new(cfg: &SyncConfig, group: &GroupKey, device: &str) -> Result<Self, SyncError> {
        match cfg.transport.as_str() {
            "folder" => Ok(Transport::Folder(Arc::new(FolderTransport::new(
                PathBuf::from(cfg.location.trim()),
                &group.group_id(),
                device,
            )))),
            "relay" => Ok(Transport::Relay(RelayTransport::new(
                &cfg.location,
                &group.group_id(),
                &group.relay_token(),
                device,
            )?)),
            other => Err(SyncError::Storage(format!("unknown sync transport {other}"))),
        }
    }

    async fn check(&self) -> Result<(), SyncError> {
        match self {
            Transport::Folder(f) => {
                let f = f.clone();
                tauri::async_runtime::spawn_blocking(move || f.check())
                    .await
                    .map_err(blocking_err)?
            }
            Transport::Relay(r) => r.pull(i64::MAX - 1).await.map(|_| ()),
        }
    }

    async fn push(&self, sealed: Sealed) -> Result<(), SyncError> {
        match self {
            Transport::Folder(f) => {
                let f = f.clone();
                tauri::async_runtime::spawn_blocking(move || f.push(&sealed))
                    .await
                    .map_err(blocking_err)?
            }
            Transport::Relay(r) => r.push(&sealed).await,
        }
    }

    /// Cursors are stored as text: a per-device map for folders, a sequence
    /// number for the relay.
    async fn pull(&self, cursor: Option<String>) -> Result<(Vec<Received>, String), SyncError> {
        match self {
            Transport::Folder(f) => {
                let f = f.clone();
                let c: FolderCursor = cursor.and_then(|c| serde_json::from_str(&c).ok()).unwrap_or_default();
                let (got, next) = tauri::async_runtime::spawn_blocking(move || f.pull(&c))
                    .await
                    .map_err(blocking_err)??;
                Ok((got, serde_json::to_string(&next).unwrap_or_default()))
            }
            Transport::Relay(r) => {
                let c = cursor.and_then(|c| c.parse().ok()).unwrap_or(0);
                let (got, next) = r.pull(c).await?;
                Ok((got, next.to_string()))
            }
        }
    }

    async fn put_device(&self, sealed: Sealed) -> Result<(), SyncError> {
        match self {
            Transport::Folder(f) => {
                let f = f.clone();
                tauri::async_runtime::spawn_blocking(move || f.put_device(&sealed))
                    .await
                    .map_err(blocking_err)?
            }
            Transport::Relay(r) => r.put_device(&sealed).await,
        }
    }

    async fn devices(&self) -> Result<Vec<Received>, SyncError> {
        match self {
            Transport::Folder(f) => {
                let f = f.clone();
                tauri::async_runtime::spawn_blocking(move || f.devices())
                    .await
                    .map_err(blocking_err)?
            }
            Transport::Relay(r) => r.devices().await,
        }
    }
}

/// Shared between the commands and the background loop.
pub struct SyncEngine {
    status: Mutex<SyncStateDto>,
    /// Other devices' latest heartbeats.
    peers: Mutex<Vec<(DeviceInfo, String)>>,
    wake: tokio::sync::Notify,
    /// Bumped on every start/stop so an old loop knows to exit.
    generation: AtomicU64,
}

impl Default for SyncEngine {
    fn default() -> Self {
        SyncEngine {
            status: Mutex::new(SyncStateDto::Off),
            peers: Mutex::new(vec![]),
            wake: tokio::sync::Notify::new(),
            generation: AtomicU64::new(0),
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex32(s: &str) -> Option<[u8; 32]> {
    let v: Vec<u8> = (0..s.len())
        .step_by(2)
        .filter_map(|i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
        .collect();
    v.try_into().ok()
}

/// Keychain entries, per device id so two installs (or test instances) on
/// one machine never share them.
fn group_secret(device: &str) -> String {
    format!("sync:{device}:group-key")
}

fn device_secret(device: &str) -> String {
    format!("sync:{device}:device-key")
}

fn this_device(app: &AppHandle) -> Option<String> {
    Some(app.state::<AppState>().lib.lock().ok()?.device().to_string())
}

fn computer_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "This computer".into())
}

impl SyncEngine {
    pub fn status(&self) -> SyncStateDto {
        self.status.lock().map(|s| s.clone()).unwrap_or(SyncStateDto::Off)
    }

    fn set_status(&self, app: &AppHandle, s: SyncStateDto) {
        if let Ok(mut cur) = self.status.lock() {
            if *cur != s {
                *cur = s;
                let _ = app.emit("sync-changed", ());
            }
        }
    }

    /// Something changed locally: sync soon rather than at the next tick.
    pub fn poke(&self) {
        self.wake.notify_one();
    }

    pub fn config(app: &AppHandle) -> Option<SyncConfig> {
        let state = app.state::<AppState>();
        let lib = state.lib.lock().ok()?;
        lib.local_setting(CONFIG_KEY)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    fn group_key(vault: &Vault, device: &str) -> Option<GroupKey> {
        vault
            .get(&group_secret(device))
            .and_then(|s| unhex32(s.expose()))
            .map(GroupKey::from_bytes)
    }

    fn device_key(vault: &Vault, device: &str) -> Result<DeviceKey, String> {
        if let Some(k) = vault.get(&device_secret(device)).and_then(|s| unhex32(s.expose())) {
            return Ok(DeviceKey::from_bytes(k));
        }
        let k = DeviceKey::generate();
        vault
            .save_hidden(&device_secret(device), Secret::new(hex(&k.to_bytes())))
            .map_err(|e| e.to_string())?;
        Ok(k)
    }

    /// The phrase, to add another computer.
    pub fn phrase(app: &AppHandle) -> Option<String> {
        Self::group_key(&app.state::<AppState>().vault, &this_device(app)?).map(|k| k.phrase())
    }

    /// Set up (or change) sync and start it. `phrase: None` creates a new
    /// group. Returns the phrase to show.
    pub async fn setup(app: &AppHandle, cfg: SyncConfig, phrase: Option<String>) -> Result<String, String> {
        let state = app.state::<AppState>();
        let joining = phrase.is_some();
        let group = match &phrase {
            Some(p) => GroupKey::from_phrase(p).map_err(|e| e.to_string())?,
            None => GroupKey::generate(),
        };
        let device = state.lib.lock().map_err(|e| e.to_string())?.device().to_string();
        let transport = Transport::new(&cfg, &group, &device).map_err(|e| e.to_string())?;
        transport.check().await.map_err(|e| e.to_string())?;
        Self::device_key(&state.vault, &device)?;
        state
            .vault
            .save_hidden(&group_secret(&device), Secret::new(hex(&group.to_bytes())))
            .map_err(|e| e.to_string())?;
        {
            let mut lib = state.lib.lock().map_err(|e| e.to_string())?;
            lib.set_local_setting(CONFIG_KEY, &serde_json::to_string(&cfg).map_err(|e| e.to_string())?)?;
            lib.set_sync_cursor("sync", "")?;
            lib.set_local_setting(CLOSED_KEY, "")?;
            // An untouched install joining a group swaps its starter folder
            // for the group's instead of adding another copy everywhere.
            let pushed = if joining { lib.drop_untouched_seed()? } else { None };
            let pushed = pushed.and_then(|h| serde_json::to_string(&h).ok()).unwrap_or_default();
            lib.set_local_setting(PUSHED_KEY, &pushed)?;
        }
        let _ = app.emit("tree-changed", ());
        Self::start(app);
        Ok(group.phrase())
    }

    pub fn disable(app: &AppHandle) -> Result<(), String> {
        let state = app.state::<AppState>();
        state.sync.generation.fetch_add(1, Ordering::SeqCst);
        state.sync.poke();
        let device = {
            let lib = state.lib.lock().map_err(|e| e.to_string())?;
            lib.set_local_setting(CONFIG_KEY, "")?;
            lib.set_local_setting(CLOSED_KEY, "")?;
            lib.device().to_string()
        };
        state
            .vault
            .forget_hidden(&group_secret(&device))
            .map_err(|e| e.to_string())?;
        if let Ok(mut p) = state.sync.peers.lock() {
            p.clear();
        }
        state.sync.set_status(app, SyncStateDto::Off);
        Ok(())
    }

    /// Replace the group with a new one (new phrase): to remove a computer,
    /// or because the phrase got out. The old group gets a notice so the
    /// other computers stop and ask for the new phrase; this computer seeds
    /// the new group with the whole history. Returns the new phrase.
    pub async fn rotate(app: &AppHandle) -> Result<String, String> {
        let state = app.state::<AppState>();
        let cfg = Self::config(app).ok_or("Sync is off")?;
        let device = this_device(app).ok_or("the library is unavailable")?;
        let old = Self::group_key(&state.vault, &device).ok_or("the sync phrase is missing from the keychain")?;
        let key = Self::device_key(&state.vault, &device)?;

        let notice = Notice {
            closed_by: computer_name(),
            at: now_ms(),
        };
        Transport::new(&cfg, &old, &device)
            .map_err(|e| e.to_string())?
            .push(seal(&old, &key, &device, &notice))
            .await
            .map_err(|e| format!("couldn't close the old sync group: {e}"))?;

        // Stop the old loop before switching keys.
        state.sync.generation.fetch_add(1, Ordering::SeqCst);
        state.sync.poke();
        let group = GroupKey::generate();
        let transport = Transport::new(&cfg, &group, &device).map_err(|e| e.to_string())?;
        transport.check().await.map_err(|e| e.to_string())?;
        let (all, last_own) = {
            let lib = state.lib.lock().map_err(|e| e.to_string())?;
            (lib.all_ops(), lib.last_own_op())
        };
        for chunk in all.chunks(BATCH_OPS) {
            transport
                .push(seal(&group, &key, &device, &Batch { ops: chunk.to_vec() }))
                .await
                .map_err(|e| e.to_string())?;
        }
        state
            .vault
            .save_hidden(&group_secret(&device), Secret::new(hex(&group.to_bytes())))
            .map_err(|e| e.to_string())?;
        {
            let lib = state.lib.lock().map_err(|e| e.to_string())?;
            lib.set_sync_cursor("sync", "")?;
            lib.set_local_setting(CLOSED_KEY, "")?;
            let pushed = last_own
                .and_then(|h| serde_json::to_string(&h).ok())
                .unwrap_or_default();
            lib.set_local_setting(PUSHED_KEY, &pushed)?;
        }
        if let Ok(mut p) = state.sync.peers.lock() {
            p.clear();
        }
        Self::start(app);
        Ok(group.phrase())
    }

    /// Start the background loop if sync is set up (called at launch and
    /// after setup).
    pub fn start(app: &AppHandle) {
        let state = app.state::<AppState>();
        let Some(cfg) = Self::config(app) else { return };
        if let Some(message) = closed_message(app) {
            state.sync.set_status(app, SyncStateDto::Error { message });
            return;
        }
        let Some(device) = this_device(app) else { return };
        let Some(group) = Self::group_key(&state.vault, &device) else {
            state.sync.set_status(
                app,
                SyncStateDto::Error {
                    message: "the sync phrase is missing from this computer's keychain; set sync up again".into(),
                },
            );
            return;
        };
        let generation = state.sync.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let state = app.state::<AppState>();
            let key = match Self::device_key(&state.vault, &device) {
                Ok(k) => k,
                Err(e) => return state.sync.set_status(&app, SyncStateDto::Error { message: e }),
            };
            let transport = match Transport::new(&cfg, &group, &device) {
                Ok(t) => t,
                Err(e) => {
                    return state
                        .sync
                        .set_status(&app, SyncStateDto::Error { message: e.to_string() })
                }
            };
            let mut last_heartbeat = 0;
            state
                .sync
                .set_status(&app, SyncStateDto::Syncing { transport: cfg.label() });
            while state.sync.generation.load(Ordering::SeqCst) == generation {
                let result = round(&app, &transport, &group, &key, &device, &mut last_heartbeat).await;
                if state.sync.generation.load(Ordering::SeqCst) != generation {
                    break;
                }
                let status = match result {
                    Ok(Some(notice)) => {
                        let message = closed_text(&notice);
                        if let Ok(lib) = state.lib.lock() {
                            let _ = lib.set_local_setting(CLOSED_KEY, &message);
                        }
                        state.sync.set_status(&app, SyncStateDto::Error { message });
                        break;
                    }
                    Ok(None) => SyncStateDto::Synced {
                        transport: cfg.label(),
                        at: now_ms(),
                    },
                    Err(SyncError::Storage(_)) | Err(SyncError::Relay(_)) if unpushed(&app).is_some() => {
                        SyncStateDto::Offline {
                            pending: unpushed(&app).unwrap_or(0) as u32,
                        }
                    }
                    Err(e) => SyncStateDto::Error { message: e.to_string() },
                };
                state.sync.set_status(&app, status);
                tokio::select! {
                    _ = tokio::time::sleep(cfg.interval()) => {}
                    _ = state.sync.wake.notified() => {}
                }
            }
        });
    }

    /// Heartbeats from other devices, for the Devices page.
    pub fn devices(app: &AppHandle) -> Vec<DeviceDto> {
        let state = app.state::<AppState>();
        let label = Self::config(app).map(|c| c.transport).unwrap_or_else(|| "relay".into());
        let peers = state.sync.peers.lock().map(|p| p.clone()).unwrap_or_default();
        peers
            .into_iter()
            .map(|(d, _)| DeviceDto {
                id: d.id,
                name: d.name,
                os: d.os,
                app_version: d.app_version,
                protocol_version: d.protocol_version,
                last_seen: d.sent_at,
                transport: label.clone(),
                pending_ops: 0,
                is_this_device: false,
            })
            .collect()
    }
}

fn closed_text(n: &Notice) -> String {
    format!(
        "{} replaced the sync phrase, so this computer stopped syncing. Enter the new phrase from {} under Settings → Sync → Use a new phrase.",
        n.closed_by, n.closed_by
    )
}

fn closed_message(app: &AppHandle) -> Option<String> {
    let state = app.state::<AppState>();
    let lib = state.lib.lock().ok()?;
    lib.local_setting(CLOSED_KEY).ok().flatten().filter(|m| !m.is_empty())
}

/// Ops waiting to be pushed (for "offline, N changes waiting").
pub fn unpushed(app: &AppHandle) -> Option<usize> {
    let state = app.state::<AppState>();
    let lib = state.lib.lock().ok()?;
    let pushed: Option<Hlc> = lib
        .local_setting(PUSHED_KEY)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok());
    Some(lib.own_ops_after(pushed.as_ref()).len())
}

/// Accept a sealed item only if its sender's key matches the one first seen
/// for that device (so a member can't later impersonate another device).
fn pinned_ok(app: &AppHandle, sealed: &Sealed) -> bool {
    let state = app.state::<AppState>();
    let Ok(lib) = state.lib.lock() else { return false };
    let slot = format!("{PEER_PREFIX}{}", sealed.device);
    match lib.local_setting(&slot).ok().flatten() {
        Some(k) if !k.is_empty() => k == sealed.key,
        _ => lib.set_local_setting(&slot, &sealed.key).is_ok(),
    }
}

async fn round(
    app: &AppHandle,
    transport: &Transport,
    group: &GroupKey,
    key: &DeviceKey,
    device: &str,
    last_heartbeat: &mut u64,
) -> Result<Option<Notice>, SyncError> {
    let state = app.state::<AppState>();
    let lib_err = |e: String| SyncError::Storage(e);

    // Push what this device changed since the last push.
    let ops: Vec<Op> = {
        let lib = state.lib.lock().map_err(|e| lib_err(e.to_string()))?;
        let pushed = lib
            .local_setting(PUSHED_KEY)
            .map_err(lib_err)?
            .and_then(|s| serde_json::from_str(&s).ok());
        lib.own_ops_after(pushed.as_ref())
    };
    for chunk in ops.chunks(BATCH_OPS) {
        transport
            .push(seal(group, key, device, &Batch { ops: chunk.to_vec() }))
            .await?;
        let done = chunk.last().map(|o| o.id.clone());
        let lib = state.lib.lock().map_err(|e| lib_err(e.to_string()))?;
        lib.set_local_setting(PUSHED_KEY, &serde_json::to_string(&done).unwrap_or_default())
            .map_err(lib_err)?;
    }

    // Pull everyone else's.
    let cursor = state
        .lib
        .lock()
        .map_err(|e| lib_err(e.to_string()))?
        .sync_cursor("sync")
        .map_err(lib_err)?
        .filter(|c| !c.is_empty());
    let (received, next) = transport.pull(cursor).await?;
    let mut incoming = Vec::new();
    let mut problem = None;
    let mut closed = None;
    for r in received {
        if r.sealed.device == device {
            continue; // the relay returns our own pushes too
        }
        if !pinned_ok(app, &r.sealed) {
            problem = Some(SyncError::Tampered);
            continue;
        }
        match open::<Batch>(group, &r.sealed) {
            Ok(b) => incoming.extend(b.ops),
            Err(e) => match open::<Notice>(group, &r.sealed) {
                Ok(n) => closed = Some(n),
                Err(_) => problem = Some(e),
            },
        }
    }
    let fresh = {
        let mut lib = state.lib.lock().map_err(|e| lib_err(e.to_string()))?;
        let fresh = lib.apply_remote(incoming).map_err(lib_err)?;
        lib.set_sync_cursor("sync", &next).map_err(lib_err)?;
        fresh
    };
    if fresh > 0 {
        let _ = app.emit("tree-changed", ());
    }
    if closed.is_some() {
        return Ok(closed);
    }

    // Our heartbeat once a minute; everyone else's every round, so a
    // computer that just joined shows up on the Devices page right away
    // (the status update that follows refreshes it).
    if now_ms().saturating_sub(*last_heartbeat) >= HEARTBEAT_EVERY {
        let info = DeviceInfo {
            id: device.to_string(),
            name: computer_name(),
            os: std::env::consts::OS.into(),
            app_version: app.package_info().version.to_string(),
            protocol_version: linecard_core_sync::SYNC_PROTOCOL_VERSION,
            sent_at: now_ms(),
        };
        transport.put_device(seal(group, key, device, &info)).await?;
        *last_heartbeat = now_ms();
    }
    let peers: Vec<(DeviceInfo, String)> = transport
        .devices()
        .await?
        .into_iter()
        .filter(|r| r.sealed.device != device)
        .filter_map(|r| Some((open::<DeviceInfo>(group, &r.sealed).ok()?, r.sealed.device)))
        .collect();
    if let Ok(mut p) = state.sync.peers.lock() {
        *p = peers;
    }

    match problem {
        Some(e) => Err(e),
        None => Ok(None),
    }
}
