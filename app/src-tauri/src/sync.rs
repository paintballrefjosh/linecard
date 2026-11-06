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
use linecard_sync::github::GitHubTransport;
use linecard_sync::lan::{LanCursor, LanTransport};
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
    /// Folder path, relay URL, or GitHub "owner/repo".
    pub location: String,
}

impl SyncConfig {
    fn label(&self) -> String {
        match self.transport.as_str() {
            "folder" => "shared folder".into(),
            "github" => "GitHub".into(),
            "lan" => "this network".into(),
            _ => "relay".into(),
        }
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(match self.transport.as_str() {
            "relay" => 5,
            // GitHub's API allows 5000 requests an hour.
            "github" => 30,
            "lan" => 3,
            _ => 10,
        })
    }
}

enum Transport {
    Folder(Arc<FolderTransport>),
    Relay(RelayTransport),
    GitHub(GitHubTransport),
    Lan(Arc<LanTransport>),
}

fn blocking_err(e: tauri::Error) -> SyncError {
    SyncError::Storage(e.to_string())
}

impl Transport {
    async fn new(
        app: &AppHandle,
        cfg: &SyncConfig,
        group: &GroupKey,
        device: &str,
        account: Option<String>,
    ) -> Result<Self, SyncError> {
        match cfg.transport.as_str() {
            "folder" => Ok(Transport::Folder(Arc::new(FolderTransport::new(
                PathBuf::from(cfg.location.trim()),
                &group.group_id(),
                device,
            )))),
            "relay" => Ok(Transport::Relay(
                RelayTransport::new(&cfg.location, &group.group_id(), &group.relay_token(), device)?
                    .with_account(account),
            )),
            "github" => Ok(Transport::GitHub(GitHubTransport::new(
                linecard_sync::github::API,
                &cfg.location,
                account.as_deref().unwrap_or(""),
                &group.group_id(),
                device,
            )?)),
            "lan" => {
                let store = app
                    .state::<AppState>()
                    .data_dir
                    .join(format!("lan-{}.db", group.group_id()));
                Ok(Transport::Lan(Arc::new(
                    LanTransport::start(Some(&store), group, device, true).await?,
                )))
            }
            other => Err(SyncError::Storage(format!("unknown sync transport {other}"))),
        }
    }

    /// Between rounds: the relay answers as soon as something new arrives
    /// (or after ~25 s); a folder is checked again after `interval`.
    async fn idle(&self, app: &AppHandle, interval: Duration) {
        match self {
            Transport::Folder(_) | Transport::GitHub(_) | Transport::Lan(_) => tokio::time::sleep(interval).await,
            Transport::Relay(r) => {
                let cursor = app
                    .state::<AppState>()
                    .lib
                    .lock()
                    .ok()
                    .and_then(|l| l.sync_cursor("sync").ok().flatten())
                    .map(|c| split_relay_cursor(&c).1)
                    .unwrap_or(0);
                if r.wait(cursor).await.is_err() {
                    // Offline or an old relay without waiting: don't spin.
                    tokio::time::sleep(interval).await;
                }
            }
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
            Transport::GitHub(g) => g.check().await,
            Transport::Lan(_) => Ok(()),
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
            Transport::GitHub(g) => g.push(&sealed).await,
            Transport::Lan(l) => l.push(&sealed),
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
                let (known, seq) = match cursor.as_deref().map(split_relay_cursor) {
                    Some((e, s)) => (e.map(str::to_string), s),
                    None => (None, 0),
                };
                let now = r.epoch().await?;
                if matches!((&known, &now), (Some(k), Some(n)) if k != n) {
                    // The operator restored a backup: the relay's history went back in
                    // time. Start over from its beginning; `round` re-sends ours.
                    return Ok((Vec::new(), join_relay_cursor(now.as_deref(), 0)));
                }
                let (got, next) = r.pull(seq).await?;
                Ok((got, join_relay_cursor(now.as_deref(), next)))
            }
            Transport::GitHub(g) => {
                let c: FolderCursor = cursor.and_then(|c| serde_json::from_str(&c).ok()).unwrap_or_default();
                let (got, next) = g.pull(&c).await?;
                Ok((got, serde_json::to_string(&next).unwrap_or_default()))
            }
            Transport::Lan(l) => {
                let c: LanCursor = cursor.and_then(|c| serde_json::from_str(&c).ok()).unwrap_or_default();
                let (got, next) = l.pull(&c).await?;
                Ok((got, serde_json::to_string(&next).unwrap_or_default()))
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
            Transport::GitHub(g) => g.put_device(&sealed).await,
            Transport::Lan(l) => l.put_device(&sealed),
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
            Transport::GitHub(g) => g.devices().await,
            Transport::Lan(l) => l.devices().await,
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

/// A private relay's access key, or the GitHub token.
fn account_secret(device: &str) -> String {
    format!("sync:{device}:relay-account")
}

fn relay_account(vault: &Vault, device: &str) -> Option<String> {
    vault.get(&account_secret(device)).map(|s| s.expose().to_string())
}

fn this_device(app: &AppHandle) -> Option<String> {
    Some(app.state::<AppState>().lib.lock().ok()?.device().to_string())
}

/// This device's heartbeat.
fn device_info(app: &AppHandle, device: &str) -> DeviceInfo {
    DeviceInfo {
        id: device.to_string(),
        name: computer_name(),
        os: std::env::consts::OS.into(),
        app_version: app.package_info().version.to_string(),
        protocol_version: linecard_core_sync::SYNC_PROTOCOL_VERSION,
        sent_at: now_ms(),
    }
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
        match &s {
            SyncStateDto::Error { message } => crate::debuglog::write("sync", message),
            SyncStateDto::Offline { pending } => {
                crate::debuglog::write("sync", &format!("offline, {pending} changes waiting"))
            }
            _ => {}
        }
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
    pub async fn setup(
        app: &AppHandle,
        cfg: SyncConfig,
        phrase: Option<String>,
        account: Option<String>,
    ) -> Result<String, String> {
        let state = app.state::<AppState>();
        let joining = phrase.is_some();
        let group = match &phrase {
            Some(p) => GroupKey::from_phrase(p).map_err(|e| e.to_string())?,
            None => GroupKey::generate(),
        };
        let device = state.lib.lock().map_err(|e| e.to_string())?.device().to_string();
        // Re-joining (a new phrase) keeps the saved access key or token.
        let account = account
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .or_else(|| relay_account(&state.vault, &device))
            .filter(|_| cfg.transport != "folder");
        let transport = Transport::new(app, &cfg, &group, &device, account.clone())
            .await
            .map_err(|e| e.to_string())?;
        transport.check().await.map_err(|e| e.to_string())?;
        let key = Self::device_key(&state.vault, &device)?;
        // A new group is claimed now (with its first heartbeat), so a private
        // relay's missing or wrong access key shows up here, not later.
        if !joining {
            transport
                .put_device(seal(&group, &key, &device, &device_info(app, &device)))
                .await
                .map_err(|e| e.to_string())?;
        }
        match &account {
            Some(a) => state
                .vault
                .save_hidden(&account_secret(&device), Secret::new(a.clone())),
            None => state.vault.forget_hidden(&account_secret(&device)),
        }
        .map_err(|e| e.to_string())?;
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
        state.sync.wake.notify_waiters();
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
        let _ = state.vault.forget_hidden(&account_secret(&device));
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
        let account = relay_account(&state.vault, &device);
        Transport::new(app, &cfg, &old, &device, account.clone())
            .await
            .map_err(|e| e.to_string())?
            .push(seal(&old, &key, &device, &notice))
            .await
            .map_err(|e| format!("couldn't close the old sync group: {e}"))?;

        // Stop the old loop before switching keys.
        state.sync.generation.fetch_add(1, Ordering::SeqCst);
        state.sync.wake.notify_waiters();
        let group = GroupKey::generate();
        let transport = Transport::new(app, &cfg, &group, &device, account)
            .await
            .map_err(|e| e.to_string())?;
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
        state.sync.wake.notify_waiters();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let state = app.state::<AppState>();
            let key = match Self::device_key(&state.vault, &device) {
                Ok(k) => k,
                Err(e) => return state.sync.set_status(&app, SyncStateDto::Error { message: e }),
            };
            let transport =
                match Transport::new(&app, &cfg, &group, &device, relay_account(&state.vault, &device)).await {
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
                // A relay that says "slow down" is obeyed: wait as long as it asks,
                // on top of the usual pause.
                let retry_after = match &result {
                    Err(SyncError::RateLimited(n)) => Some((*n).min(600)),
                    _ => None,
                };
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
                if let Some(secs) = retry_after {
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                }
                tokio::select! {
                    _ = transport.idle(&app, cfg.interval()) => {}
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

/// The relay's cursor is "epoch:seq"; a bare "seq" is from before epochs.
fn split_relay_cursor(c: &str) -> (Option<&str>, i64) {
    match c.rsplit_once(':') {
        Some((epoch, seq)) => (Some(epoch), seq.parse().unwrap_or(0)),
        None => (None, c.parse().unwrap_or(0)),
    }
}

fn join_relay_cursor(epoch: Option<&str>, seq: i64) -> String {
    match epoch {
        Some(e) => format!("{e}:{seq}"),
        None => seq.to_string(),
    }
}

/// True when moving from cursor `old` to `new` crossed a restore of the relay
/// (both name an epoch, and it differs).
fn relay_restored(old: Option<&str>, new: &str) -> bool {
    match (old.map(split_relay_cursor), split_relay_cursor(new)) {
        (Some((Some(a), _)), (Some(b), _)) => a != b,
        _ => false,
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
/// Call only after `open` succeeded, so only verified keys get pinned.
fn pinned_ok(app: &AppHandle, sealed: &Sealed) -> bool {
    let state = app.state::<AppState>();
    let Ok(lib) = state.lib.lock() else { return false };
    let slot = format!("{PEER_PREFIX}{}", sealed.device);
    match lib.local_setting(&slot).ok().flatten() {
        Some(k) if !k.is_empty() => k == sealed.key,
        _ => lib.set_local_setting(&slot, &sealed.key).is_ok(),
    }
}

// ---- relay compaction (see docs/sync-protocol.md) ----------------------------------

/// A relay group this big (in batches) is worth compacting.
const COMPACT_AT_BATCHES: i64 = 500;
/// Don't try more often than this.
const COMPACT_EVERY: u64 = 60 * 60 * 1000;
/// Computers that reported in this recently take part in choosing who compacts.
const ACTIVE_WINDOW: u64 = 7 * 24 * 60 * 60 * 1000;
/// A snapshot batch stays well under the relay's 1 MiB limit.
const SNAPSHOT_BATCH_BYTES: usize = 600_000;

static LAST_COMPACT: AtomicU64 = AtomicU64::new(0);

/// Only one computer compacts: the one with the lowest id among those seen
/// lately (so two computers don't do it at once).
fn is_compactor(me: &str, peers: &[(DeviceInfo, String)], now: u64) -> bool {
    peers
        .iter()
        .filter(|(info, _)| now.saturating_sub(info.sent_at) <= ACTIVE_WINDOW)
        .all(|(_, id)| id.as_str() > me)
}

/// Snapshot ops in batches of at most `BATCH_OPS` ops and about
/// `SNAPSHOT_BATCH_BYTES` of JSON.
fn snapshot_chunks(ops: Vec<Op>) -> Vec<Vec<Op>> {
    let mut chunks: Vec<Vec<Op>> = Vec::new();
    let mut size = 0;
    for op in ops {
        let len = serde_json::to_string(&op).map_or(0, |s| s.len());
        match chunks.last_mut() {
            Some(c) if c.len() < BATCH_OPS && size + len <= SNAPSHOT_BATCH_BYTES => {
                c.push(op);
                size += len;
            }
            _ => {
                chunks.push(vec![op]);
                size = len;
            }
        }
    }
    chunks
}

/// After a round that pulled to the relay's newest batch: if the group has
/// grown large and this is the computer that compacts, replace the old
/// batches with a snapshot. Best effort; the next round carries on either way.
async fn maybe_compact(
    app: &AppHandle,
    transport: &Transport,
    group: &GroupKey,
    key: &DeviceKey,
    device: &str,
    applied_through: i64,
) {
    let Transport::Relay(relay) = transport else { return };
    let now = now_ms();
    if now.saturating_sub(LAST_COMPACT.load(Ordering::Relaxed)) < COMPACT_EVERY {
        return;
    }
    let state = app.state::<AppState>();
    let chosen = state
        .sync
        .peers
        .lock()
        .map(|p| is_compactor(device, &p, now))
        .unwrap_or(false);
    if !chosen {
        return;
    }
    let stats = match relay.stats().await {
        Ok(Some(s)) => s,
        _ => return, // an older relay, or a hiccup
    };
    // Everything on the relay must already be in this computer's library.
    if stats.batches < COMPACT_AT_BATCHES || stats.head != applied_through {
        return;
    }
    LAST_COMPACT.store(now, Ordering::Relaxed);
    let snapshot = match state.lib.lock() {
        Ok(lib) => lib.snapshot_ops(),
        Err(_) => return,
    };
    let sealed: Vec<Sealed> = snapshot_chunks(snapshot)
        .into_iter()
        .map(|ops| seal(group, key, device, &Batch { ops }))
        .collect();
    match relay.compact(applied_through, &sealed).await {
        Ok(removed) => crate::debuglog::write(
            "sync",
            &format!(
                "compacted the relay: {removed} batches became {} snapshot batches",
                sealed.len()
            ),
        ),
        Err(e) => crate::debuglog::write("sync", &format!("relay compaction failed: {e}")),
    }
}

/// What a pull turned out to hold.
#[derive(Default)]
struct Sorted {
    incoming: Vec<Op>,
    closed: Option<Notice>,
    problem: Option<SyncError>,
    /// An item came from a newer Linecard: don't move the cursor past it.
    hold: bool,
}

/// Open each pulled item, keeping changes and notices from verified members.
/// Forged or foreign items are reported and skipped for good (retrying can't
/// fix them, and they mustn't stall sync). Items signed by a member that this
/// version can't read are reported and held for after an update.
fn sort_received(
    group: &GroupKey,
    device: &str,
    received: Vec<Received>,
    mut pinned_ok: impl FnMut(&Sealed) -> bool,
) -> Sorted {
    let mut out = Sorted::default();
    for r in received {
        if r.sealed.device == device {
            continue; // the relay returns our own pushes too
        }
        // Verify first, pin after: pinning an unverified key would let anyone
        // who can write to the storage (without the phrase) claim a device's
        // name before that device first syncs, and lock it out for good.
        let opened = match open::<Batch>(group, &r.sealed) {
            Ok(b) => Ok(Ok(b)),
            Err(e) => open::<Notice>(group, &r.sealed).map(Err).map_err(|_| e),
        };
        match opened {
            Err(e) => {
                // Corrupt here means it verified and decrypted but didn't parse:
                // a format from a newer version, like Unsupported.
                out.hold |= matches!(e, SyncError::Unsupported(_) | SyncError::Corrupt(_));
                out.problem = Some(e);
            }
            Ok(_) if !pinned_ok(&r.sealed) => out.problem = Some(SyncError::Tampered),
            Ok(Ok(b)) => out.incoming.extend(b.ops),
            Ok(Err(n)) => out.closed = Some(n),
        }
    }
    out
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
    let (received, next) = transport.pull(cursor.clone()).await?;
    if matches!(transport, Transport::Relay(_)) && relay_restored(cursor.as_deref(), &next) {
        // Its history went back in time (a restored backup): what we had already
        // sent may be gone. Send everything again (other computers skip what they
        // have), and pull its history from the start on the next round.
        let lib = state.lib.lock().map_err(|e| lib_err(e.to_string()))?;
        lib.set_local_setting(PUSHED_KEY, "null").map_err(lib_err)?;
        lib.set_sync_cursor("sync", &next).map_err(lib_err)?;
        drop(lib);
        crate::debuglog::write(
            "sync",
            "the relay was restored from a backup: sending this computer's changes again",
        );
        state.sync.poke();
        return Ok(None);
    }
    let Sorted {
        incoming,
        closed,
        problem,
        hold,
    } = sort_received(group, device, received, |s| pinned_ok(app, s));
    let fresh = {
        let mut lib = state.lib.lock().map_err(|e| lib_err(e.to_string()))?;
        let fresh = lib.apply_remote(incoming).map_err(lib_err)?;
        // Something this version can't read yet: keep the cursor where it was so
        // it's pulled again after an update (what did open is skipped as seen).
        if !hold {
            lib.set_sync_cursor("sync", &next).map_err(lib_err)?;
        }
        fresh
    };
    if fresh > 0 {
        let _ = app.emit("tree-changed", ());
        // Passwords saved or removed on another computer.
        let _ = crate::secrets::reconcile(app);
    }
    if closed.is_some() {
        return Ok(closed);
    }

    // Our heartbeat once a minute; everyone else's every round, so a
    // computer that just joined shows up on the Devices page right away
    // (the status update that follows refreshes it).
    if now_ms().saturating_sub(*last_heartbeat) >= HEARTBEAT_EVERY {
        transport
            .put_device(seal(group, key, device, &device_info(app, device)))
            .await?;
        *last_heartbeat = now_ms();
    }
    let peers: Vec<(DeviceInfo, String)> = transport
        .devices()
        .await?
        .into_iter()
        .filter(|r| r.sealed.device != device)
        .filter_map(|r| {
            let info = open::<DeviceInfo>(group, &r.sealed).ok()?;
            pinned_ok(app, &r.sealed).then_some((info, r.sealed.device))
        })
        .collect();
    if let Ok(mut p) = state.sync.peers.lock() {
        *p = peers;
    }

    if problem.is_none() {
        maybe_compact(app, transport, group, key, device, split_relay_cursor(&next).1).await;
    }

    match problem {
        Some(e) => Err(e),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn got(sealed: Sealed) -> Received {
        Received { sealed, stored_at: 0 }
    }

    #[test]
    fn forged_items_are_skipped_and_never_pinned() {
        let (group, other) = (GroupKey::generate(), GroupKey::generate());
        let k = DeviceKey::generate();
        let received = vec![
            got(seal(&other, &k, "laptop", &Batch { ops: vec![] })),
            got(seal(&group, &k, "me", &Batch { ops: vec![] })),
            got(seal(
                &group,
                &k,
                "desk",
                &Notice {
                    closed_by: "desk".into(),
                    at: 1,
                },
            )),
        ];
        let mut asked = Vec::new();
        let s = sort_received(&group, "me", received, |s| {
            asked.push(s.device.clone());
            true
        });
        assert_eq!(
            asked,
            ["desk"],
            "only verified senders get pinned; our own items are skipped"
        );
        assert!(matches!(s.problem, Some(SyncError::WrongKey | SyncError::Tampered)));
        assert!(!s.hold, "a forgery can't be fixed by retrying, so the cursor moves on");
        assert_eq!(s.closed.map(|n| n.closed_by).as_deref(), Some("desk"));
    }

    #[test]
    fn a_key_that_doesnt_match_the_pinned_one_is_refused() {
        let group = GroupKey::generate();
        let k = DeviceKey::generate();
        let s = sort_received(
            &group,
            "me",
            vec![got(seal(&group, &k, "desk", &Batch { ops: vec![] }))],
            |_| false,
        );
        assert!(matches!(s.problem, Some(SyncError::Tampered)));
        assert!(!s.hold);
    }

    #[test]
    fn items_from_a_newer_version_hold_the_cursor() {
        let group = GroupKey::generate();
        let k = DeviceKey::generate();
        // Signed and encrypted by a member, but in a shape this version doesn't know.
        let newer = serde_json::json!({ "ops": [{ "kind": "from the future" }] });
        let s = sort_received(&group, "me", vec![got(seal(&group, &k, "desk", &newer))], |_| true);
        assert!(matches!(s.problem, Some(SyncError::Corrupt(_))));
        assert!(s.hold);

        let mut v2 = seal(&group, &k, "desk", &Batch { ops: vec![] });
        v2.v = 2;
        let s = sort_received(&group, "me", vec![got(v2)], |_| true);
        assert!(matches!(s.problem, Some(SyncError::Unsupported(2))));
        assert!(s.hold);
    }

    fn peer(id: &str, age_ms: u64) -> (DeviceInfo, String) {
        (
            DeviceInfo {
                id: id.into(),
                name: id.into(),
                os: "x".into(),
                app_version: "1".into(),
                protocol_version: 1,
                sent_at: 10 * ACTIVE_WINDOW - age_ms,
            },
            id.into(),
        )
    }

    #[test]
    fn the_lowest_recently_seen_computer_compacts() {
        let now = 10 * ACTIVE_WINDOW;
        assert!(is_compactor("b", &[], now), "alone");
        assert!(is_compactor("b", &[peer("c", 0), peer("d", 0)], now));
        assert!(!is_compactor("b", &[peer("a", 0), peer("c", 0)], now));
        // A computer nobody has heard from for a week doesn't hold the job.
        assert!(is_compactor("b", &[peer("a", ACTIVE_WINDOW + 1)], now));
    }

    #[test]
    fn snapshots_are_cut_into_relay_sized_batches() {
        use linecard_core_sync::Replica;
        let mut r = Replica::new("A");
        for i in 0..1000 {
            let f = [("name".to_string(), linecard_core_sync::Value::from(format!("c{i}")))];
            r.create(&format!("o{i}"), "connection", f.into_iter().collect(), 100 + i);
        }
        let chunks = snapshot_chunks(r.snapshot());
        assert_eq!(chunks.iter().map(Vec::len).sum::<usize>(), 1000);
        assert!(chunks.iter().all(|c| c.len() <= BATCH_OPS));
        assert_eq!(chunks.len(), 3);
        // A few huge ops split by size, not count.
        let big = |n: u64| {
            let f = [("blob".to_string(), linecard_core_sync::Value::from("z".repeat(400_000)))];
            r.clone()
                .create(&format!("big{n}"), "connection", f.into_iter().collect(), 5000 + n)
        };
        let chunks = snapshot_chunks(vec![big(1), big(2), big(3)]);
        assert_eq!(chunks.len(), 3, "400 KB each: no two fit under 600 KB");
    }
    #[test]
    fn relay_cursors_carry_the_epoch_and_still_read_old_ones() {
        assert_eq!(split_relay_cursor("a1b2c3:42"), (Some("a1b2c3"), 42));
        assert_eq!(split_relay_cursor("42"), (None, 42), "a cursor from before epochs");
        assert_eq!(split_relay_cursor(""), (None, 0));
        assert_eq!(join_relay_cursor(Some("e"), 7), "e:7");
        assert_eq!(join_relay_cursor(None, 7), "7");
        for (e, s) in [(Some("abc"), 0), (None, 9), (Some("f00"), 123_456)] {
            assert_eq!(split_relay_cursor(&join_relay_cursor(e, s)), (e, s));
        }
    }

    #[test]
    fn only_a_changed_epoch_counts_as_a_restore() {
        // Same history, a first sight of the relay, an old relay: nothing to do.
        assert!(!relay_restored(Some("a:60"), "a:70"));
        assert!(!relay_restored(None, "a:0"), "first sync adopts the epoch");
        assert!(
            !relay_restored(Some("60"), "a:60"),
            "a cursor from before epochs adopts it too"
        );
        assert!(
            !relay_restored(Some("a:60"), "70"),
            "a relay that stopped reporting one"
        );
        // The operator restored a backup.
        assert!(relay_restored(Some("a:60"), "b:0"));
    }
    #[test]
    fn keys_round_trip_through_hex() {
        let key: [u8; 32] = std::array::from_fn(|i| (i * 8) as u8);
        let text = hex(&key);
        assert_eq!(text.len(), 64);
        assert_eq!(unhex32(&text), Some(key));
        assert_eq!(unhex32(&text.to_uppercase()), Some(key));
    }

    #[test]
    fn a_damaged_hex_key_is_refused() {
        let good = hex(&[7u8; 32]);
        assert_eq!(unhex32(""), None);
        assert_eq!(unhex32(&good[..62]), None, "too short");
        assert_eq!(unhex32(&format!("{good}00")), None, "too long");
        assert_eq!(unhex32(&good.replacen("07", "zz", 1)), None, "not hex");
        assert_eq!(unhex32(&good[..63]), None, "odd length");
    }

    #[test]
    fn each_install_keeps_its_own_keychain_entries() {
        assert_eq!(group_secret("d1"), "sync:d1:group-key");
        assert_eq!(device_secret("d1"), "sync:d1:device-key");
        assert_eq!(account_secret("d1"), "sync:d1:relay-account");
        assert_ne!(group_secret("d1"), group_secret("d2"));
    }

    #[test]
    fn transports_are_named_and_polled_at_their_own_pace() {
        let cfg = |t: &str| SyncConfig {
            transport: t.into(),
            location: String::new(),
        };
        assert_eq!(cfg("folder").label(), "shared folder");
        assert_eq!(cfg("github").label(), "GitHub");
        assert_eq!(cfg("lan").label(), "this network");
        assert_eq!(cfg("relay").label(), "relay");
        assert_eq!(cfg("lan").interval(), Duration::from_secs(3));
        assert_eq!(cfg("relay").interval(), Duration::from_secs(5));
        assert_eq!(cfg("folder").interval(), Duration::from_secs(10));
        assert_eq!(cfg("github").interval(), Duration::from_secs(30));
    }

    #[test]
    fn the_closed_notice_names_who_to_ask() {
        let text = closed_text(&Notice {
            closed_by: "Office PC".into(),
            at: 0,
        });
        assert!(text.starts_with("Office PC replaced the sync phrase"));
        assert!(text.contains("from Office PC under Settings"));
    }
}
