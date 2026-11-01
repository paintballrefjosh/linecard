//! Linecard relay: stores and forwards end-to-end encrypted sync data.
//!
//! The relay never sees plaintext. A sync group is known only by a group id
//! derived from its key; clients prove membership with a bearer token, also
//! derived from the key, which the relay stores only as a SHA-256 hash. The
//! first device to write to a group claims it with its token; after that,
//! requests without the same token are refused.
//!
//! An open relay (the default) lets anyone with a phrase start a group. A
//! private relay (`open = false`) only lets accounts start groups: the
//! operator creates an account with the admin CLI and gives its access key
//! to the user, whose app sends it as `X-Linecard-Account`. Joining an
//! existing group still only takes the group's phrase.
//!
//! ```text
//! GET  /health
//! POST /v1/groups/{group}/ops                {device, blob}   -> {seq}
//! GET  /v1/groups/{group}/ops?after=N        -> {ops: [{seq, device, blob, at}], more}
//! GET  /v1/groups/{group}/ops/wait?after=N   -> {latest}  (returns early when something arrives)
//! PUT  /v1/groups/{group}/devices/{device}   {blob}           heartbeat
//! GET  /v1/groups/{group}/devices            -> {devices: [{device, blob, at}]}
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

/// Response header naming the database's epoch (see `add_epoch`).
pub const EPOCH_HEADER: &str = "x-linecard-epoch";

/// Largest single blob (one sealed batch or heartbeat).
pub const MAX_BLOB: usize = 1 << 20;
/// Batches returned per pull; clients ask again while `more` is true.
const PAGE: i64 = 500;
/// Longest a wait request is held open.
pub const MAX_WAIT: Duration = Duration::from_secs(25);

#[derive(Debug, PartialEq, Eq)]
pub enum RelayError {
    BadRequest(&'static str),
    Unauthorized,
    /// A private relay, and no (valid) account key to start a group.
    NeedsAccount,
    TooLarge,
    Full,
    Internal(String),
}

impl IntoResponse for RelayError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            RelayError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.to_string()),
            RelayError::Unauthorized => (StatusCode::UNAUTHORIZED, "wrong token for this group".into()),
            RelayError::NeedsAccount => (
                StatusCode::FORBIDDEN,
                "this relay is private: starting a sync group needs an access key from its operator".into(),
            ),
            RelayError::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "blob too large".into()),
            RelayError::Full => (
                StatusCode::INSUFFICIENT_STORAGE,
                "this group is over its storage limit".into(),
            ),
            RelayError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (status, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

impl From<rusqlite::Error> for RelayError {
    fn from(e: rusqlite::Error) -> Self {
        RelayError::Internal(e.to_string())
    }
}

type Result<T> = std::result::Result<T, RelayError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredOp {
    pub seq: i64,
    pub device: String,
    pub blob: String,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredDevice {
    pub device: String,
    pub blob: String,
    pub at: i64,
}

/// Who may start groups, and how much each group may keep.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Batches kept per group before writes are refused.
    pub max_ops: i64,
    /// Anyone may start a group (no account needed).
    pub open: bool,
    /// Bytes of batches kept per group before writes are refused (0 = no limit).
    pub max_group_bytes: i64,
    /// Groups with no write or heartbeat for this many days are deleted by the
    /// daily maintenance (0 = keep forever).
    pub idle_days: i64,
    /// Requests per second allowed per client address and per group (`None` = no limit).
    pub rate: Option<Rate>,
    /// Take the client address from `X-Forwarded-For` (only behind your own proxy).
    pub trust_proxy: bool,
}

/// A heartbeat older than this is a computer nobody uses any more.
pub const DEVICE_STALE_DAYS: i64 = 90;

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_ops: 200_000,
            open: true,
            max_group_bytes: 256 << 20,
            idle_days: 0,
            rate: None,
            trust_proxy: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Account {
    pub name: String,
    pub created: i64,
    /// Groups this account may start (`None` = no limit).
    pub max_groups: Option<i64>,
    pub groups: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupInfo {
    pub id: String,
    pub account: Option<String>,
    pub created: i64,
    pub ops: i64,
    pub devices: i64,
    /// Last write, Unix ms.
    pub last: Option<i64>,
    /// Bytes of batches stored.
    pub bytes: i64,
    /// Last sign of life (a write, a heartbeat, or creation), Unix ms.
    pub active: i64,
}

/// What one round of [`Relay::maintain`] removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Maintenance {
    pub expired: Vec<String>,
    pub devices_removed: usize,
}

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

pub struct Relay {
    /// The database file (`None` for an in-memory relay), so a backup can read it on its own connection.
    path: Option<PathBuf>,
    /// Identifies this database's history; replaced when a backup is restored.
    epoch: String,
    guard: Guard,
    metrics: Metrics,
    db: Mutex<Connection>,
    limits: Limits,
    /// Wakes waiters on a group when something is pushed to it.
    waiters: Mutex<HashMap<String, Arc<Notify>>>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn valid_group(g: &str) -> bool {
    g.len() == 32 && g.bytes().all(|b| b.is_ascii_hexdigit())
}

fn valid_device(d: &str) -> bool {
    !d.is_empty() && d.len() <= 64 && d.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Relay {
    pub fn open(path: &Path, limits: Limits) -> rusqlite::Result<Self> {
        let mut relay = Self::init(Connection::open(path)?, limits)?;
        relay.path = Some(path.to_path_buf());
        Ok(relay)
    }

    /// Fold the write-ahead log into the database file (done on shutdown, so
    /// a copied or backed-up file is complete).
    pub fn checkpoint(&self) -> rusqlite::Result<()> {
        let db = self.db.lock().map_err(|_| rusqlite::Error::InvalidQuery)?;
        db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
    }

    pub fn in_memory(limits: Limits) -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?, limits)
    }

    fn init(db: Connection, limits: Limits) -> rusqlite::Result<Self> {
        db.pragma_update(None, "journal_mode", "WAL")?;
        // WAL with NORMAL syncing can lose the last few commits on power loss
        // (never corrupts); those are sync batches a device pushes again.
        db.pragma_update(None, "synchronous", "NORMAL")?;
        db.busy_timeout(Duration::from_secs(5))?;
        migrate(&db)?;
        let epoch: String = db.query_row("SELECT value FROM meta WHERE key = 'epoch'", [], |r| r.get(0))?;
        Ok(Relay {
            path: None,
            epoch,
            guard: Guard::new(limits.rate, limits.trust_proxy),
            metrics: Metrics::default(),
            db: Mutex::new(db),
            limits,
            waiters: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn guard(&self) -> &Guard {
        &self.guard
    }

    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// What the database holds (cheap lookups, not scans of the data), for metrics.
    pub fn stored(&self) -> Result<Stored> {
        let db = self.db()?;
        let one = |sql: &str| -> rusqlite::Result<i64> { db.query_row(sql, [], |r| r.get(0)) };
        Ok(Stored {
            groups: one("SELECT COUNT(*) FROM groups")?,
            accounts: one("SELECT COUNT(*) FROM accounts")?,
            // Batches are numbered 1..n within a group, so the newest number is the count.
            batches: one("SELECT COALESCE(SUM((SELECT MAX(seq) FROM ops WHERE group_id = g.id)), 0) FROM groups g")?,
            bytes: one("SELECT COALESCE(SUM(bytes), 0) FROM groups")?,
            devices: one("SELECT COUNT(*) FROM devices")?,
            db_bytes: self
                .path
                .as_ref()
                .and_then(|p| std::fs::metadata(p).ok())
                .map_or(0, |m| m.len()),
        })
    }

    /// Ready to serve: the database answers and its schema is the one this
    /// relay expects.
    pub fn ready(&self) -> std::result::Result<(), String> {
        let db = self.db().map_err(|_| "database lock poisoned".to_string())?;
        let version: i64 = db
            .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_version", [], |r| r.get(0))
            .map_err(|e| format!("database not answering: {e}"))?;
        if version == SCHEMA_VERSION {
            Ok(())
        } else {
            Err(format!("schema version {version}, expected {SCHEMA_VERSION}"))
        }
    }

    /// Which database history this is (changes when a backup is restored).
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    fn db(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.db
            .lock()
            .map_err(|_| RelayError::Internal("database lock poisoned".into()))
    }

    /// Check the token; the first writer of a new group claims it (on a
    /// private relay only with an account key, counted against its limit).
    fn authorize(
        &self,
        db: &Connection,
        group: &str,
        token: &str,
        account: Option<&str>,
        may_claim: bool,
    ) -> Result<()> {
        if !valid_group(group) {
            return Err(RelayError::BadRequest("bad group id"));
        }
        if token.len() < 16 {
            return Err(RelayError::Unauthorized);
        }
        let stored: Option<String> = db
            .query_row("SELECT token_hash FROM groups WHERE id = ?1", [group], |r| r.get(0))
            .optional()?;
        match stored {
            Some(h) if h == hash(token) => Ok(()),
            Some(_) => Err(RelayError::Unauthorized),
            None if may_claim => {
                let owner: Option<(String, Option<i64>)> = match account.filter(|k| !k.is_empty()) {
                    Some(key) => db
                        .query_row(
                            "SELECT name, max_groups FROM accounts WHERE key_hash = ?1",
                            [hash(key)],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()?,
                    None => None,
                };
                if !self.limits.open && owner.is_none() {
                    return Err(RelayError::NeedsAccount);
                }
                if let Some((name, Some(max))) = &owner {
                    let n: i64 =
                        db.query_row("SELECT COUNT(*) FROM groups WHERE account = ?1", [name], |r| r.get(0))?;
                    if n >= *max {
                        return Err(RelayError::Full);
                    }
                }
                db.execute(
                    "INSERT INTO groups (id, token_hash, created, account) VALUES (?1, ?2, ?3, ?4)",
                    params![group, hash(token), now_ms(), owner.map(|o| o.0)],
                )?;
                Ok(())
            }
            // Reading a group nobody has written to yet: nothing to see.
            None => Ok(()),
        }
    }

    fn waiter(&self, group: &str) -> Arc<Notify> {
        let mut w = self.waiters.lock().unwrap_or_else(|p| p.into_inner());
        w.entry(group.to_string()).or_default().clone()
    }

    pub fn push(&self, group: &str, token: &str, account: Option<&str>, device: &str, blob: &str) -> Result<i64> {
        if !valid_device(device) {
            return Err(RelayError::BadRequest("bad device id"));
        }
        if blob.len() > MAX_BLOB {
            return Err(RelayError::TooLarge);
        }
        let seq = {
            let mut db = self.db()?;
            self.authorize(&db, group, token, account, true)?;
            let tx = db.transaction()?;
            let (count, last): (i64, Option<i64>) =
                tx.query_row("SELECT COUNT(*), MAX(seq) FROM ops WHERE group_id = ?1", [group], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            if count >= self.limits.max_ops {
                return Err(RelayError::Full);
            }
            let bytes: i64 = tx.query_row("SELECT bytes FROM groups WHERE id = ?1", [group], |r| r.get(0))?;
            if self.limits.max_group_bytes > 0 && bytes + blob.len() as i64 > self.limits.max_group_bytes {
                return Err(RelayError::Full);
            }
            let seq = last.unwrap_or(0) + 1;
            tx.execute(
                "INSERT INTO ops (group_id, seq, device, blob, at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![group, seq, device, blob, now_ms()],
            )?;
            tx.execute(
                "UPDATE groups SET bytes = bytes + ?2, updated = ?3 WHERE id = ?1",
                params![group, blob.len() as i64, now_ms()],
            )?;
            tx.commit()?;
            seq
        };
        self.waiter(group).notify_waiters();
        Ok(seq)
    }

    /// Batches after `after`, oldest first; `true` if there are more.
    pub fn pull(&self, group: &str, token: &str, after: i64) -> Result<(Vec<StoredOp>, bool)> {
        let db = self.db()?;
        self.authorize(&db, group, token, None, false)?;
        let mut stmt = db.prepare_cached(
            "SELECT seq, device, blob, at FROM ops WHERE group_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let mut ops: Vec<StoredOp> = stmt
            .query_map(params![group, after, PAGE + 1], |r| {
                Ok(StoredOp {
                    seq: r.get(0)?,
                    device: r.get(1)?,
                    blob: r.get(2)?,
                    at: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let more = ops.len() as i64 > PAGE;
        ops.truncate(PAGE as usize);
        Ok((ops, more))
    }

    /// The group's newest sequence number.
    pub fn latest(&self, group: &str, token: &str) -> Result<i64> {
        let db = self.db()?;
        self.authorize(&db, group, token, None, false)?;
        Ok(db.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM ops WHERE group_id = ?1",
            [group],
            |r| r.get(0),
        )?)
    }

    /// The newest sequence number once it's past `after`, or after `wait`
    /// (whichever is first).
    pub async fn wait(&self, group: &str, token: &str, after: i64, wait: Duration) -> Result<i64> {
        let notify = self.waiter(group);
        let changed = notify.notified();
        tokio::pin!(changed);
        // Register before checking, so a push in between isn't missed.
        changed.as_mut().enable();
        let latest = self.latest(group, token)?;
        if latest > after {
            return Ok(latest);
        }
        let _ = tokio::time::timeout(wait.min(MAX_WAIT), changed).await;
        self.latest(group, token)
    }

    pub fn put_device(&self, group: &str, token: &str, account: Option<&str>, device: &str, blob: &str) -> Result<()> {
        if !valid_device(device) {
            return Err(RelayError::BadRequest("bad device id"));
        }
        if blob.len() > MAX_BLOB {
            return Err(RelayError::TooLarge);
        }
        let db = self.db()?;
        self.authorize(&db, group, token, account, true)?;
        db.execute(
            "INSERT INTO devices (group_id, device, blob, at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(group_id, device) DO UPDATE SET blob = excluded.blob, at = excluded.at",
            params![group, device, blob, now_ms()],
        )?;
        Ok(())
    }

    pub fn devices(&self, group: &str, token: &str) -> Result<Vec<StoredDevice>> {
        let db = self.db()?;
        self.authorize(&db, group, token, None, false)?;
        let mut stmt = db.prepare_cached("SELECT device, blob, at FROM devices WHERE group_id = ?1 ORDER BY device")?;
        let rows = stmt
            .query_map([group], |r| {
                Ok(StoredDevice {
                    device: r.get(0)?,
                    blob: r.get(1)?,
                    at: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    // ---- administration (the CLI) -----------------------------------------------

    /// Create an account; returns its access key (shown once, stored hashed).
    pub fn add_account(&self, name: &str, max_groups: Option<i64>) -> Result<String> {
        if name.trim().is_empty() {
            return Err(RelayError::BadRequest("name the account"));
        }
        let mut raw = [0u8; 24];
        getrandom::getrandom(&mut raw).map_err(|e| RelayError::Internal(e.to_string()))?;
        let key = format!("lcr_{}", raw.iter().map(|b| format!("{b:02x}")).collect::<String>());
        let db = self.db()?;
        let n = db.execute(
            "INSERT OR IGNORE INTO accounts (name, key_hash, created, max_groups) VALUES (?1, ?2, ?3, ?4)",
            params![name.trim(), hash(&key), now_ms(), max_groups],
        )?;
        if n == 0 {
            return Err(RelayError::BadRequest("an account with that name exists"));
        }
        Ok(key)
    }

    pub fn accounts(&self) -> Result<Vec<Account>> {
        let db = self.db()?;
        let mut stmt = db.prepare(
            "SELECT a.name, a.created, a.max_groups, (SELECT COUNT(*) FROM groups g WHERE g.account = a.name)
             FROM accounts a ORDER BY a.name",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Account {
                    name: r.get(0)?,
                    created: r.get(1)?,
                    max_groups: r.get(2)?,
                    groups: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Remove an account, and with `with_groups` everything its groups hold.
    /// Returns how many groups were removed.
    pub fn remove_account(&self, name: &str, with_groups: bool) -> Result<usize> {
        let groups: Vec<String> = {
            let db = self.db()?;
            if db.execute("DELETE FROM accounts WHERE name = ?1", [name])? == 0 {
                return Err(RelayError::BadRequest("no such account"));
            }
            let mut stmt = db.prepare("SELECT id FROM groups WHERE account = ?1")?;
            let ids = stmt.query_map([name], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            ids
        };
        if !with_groups {
            return Ok(0);
        }
        for g in &groups {
            self.remove_group(g)?;
        }
        Ok(groups.len())
    }

    pub fn groups(&self) -> Result<Vec<GroupInfo>> {
        let db = self.db()?;
        let mut stmt = db.prepare(
            "SELECT g.id, g.account, g.created,
                    (SELECT COUNT(*) FROM ops o WHERE o.group_id = g.id),
                    (SELECT COUNT(*) FROM devices d WHERE d.group_id = g.id),
                    NULLIF(g.updated, 0), g.bytes,
                    MAX(g.created, g.updated, COALESCE((SELECT MAX(at) FROM devices d WHERE d.group_id = g.id), 0))
             FROM groups g ORDER BY g.created",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(GroupInfo {
                    id: r.get(0)?,
                    account: r.get(1)?,
                    created: r.get(2)?,
                    ops: r.get(3)?,
                    devices: r.get(4)?,
                    last: r.get(5)?,
                    bytes: r.get(6)?,
                    active: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Groups with no write or heartbeat for `days` days, oldest first.
    pub fn idle_groups(&self, days: i64) -> Result<Vec<GroupInfo>> {
        let cutoff = now_ms() - days.max(0) * DAY_MS;
        let mut idle: Vec<GroupInfo> = self.groups()?.into_iter().filter(|g| g.active < cutoff).collect();
        idle.sort_by_key(|g| g.active);
        Ok(idle)
    }

    /// The routine cleanup (run once a day): delete groups idle past
    /// `limits.idle_days` (when set) and heartbeats from computers unseen for
    /// [`DEVICE_STALE_DAYS`].
    pub fn maintain(&self) -> Result<Maintenance> {
        let mut expired = Vec::new();
        if self.limits.idle_days > 0 {
            for g in self.idle_groups(self.limits.idle_days)? {
                self.remove_group(&g.id)?;
                expired.push(g.id);
            }
        }
        let devices_removed = self.db()?.execute(
            "DELETE FROM devices WHERE at < ?1",
            [now_ms() - DEVICE_STALE_DAYS * DAY_MS],
        )?;
        Ok(Maintenance {
            expired,
            devices_removed,
        })
    }

    /// Rebuild the database file to give deleted data's space back to the
    /// system. Returns the bytes that were free inside the file.
    pub fn vacuum(&self) -> Result<i64> {
        let db = self.db()?;
        let free: i64 = db.query_row(
            "SELECT freelist_count * page_size FROM pragma_freelist_count, pragma_page_size",
            [],
            |r| r.get(0),
        )?;
        db.execute_batch("VACUUM")?;
        Ok(free)
    }

    pub fn remove_group(&self, id: &str) -> Result<()> {
        let mut db = self.db()?;
        let tx = db.transaction()?;
        let n = tx.execute("DELETE FROM groups WHERE id = ?1", [id])?;
        tx.execute("DELETE FROM ops WHERE group_id = ?1", [id])?;
        tx.execute("DELETE FROM devices WHERE group_id = ?1", [id])?;
        tx.commit()?;
        drop(db);
        self.waiters.lock().unwrap_or_else(|p| p.into_inner()).remove(id);
        if n == 0 {
            return Err(RelayError::BadRequest("no such group"));
        }
        Ok(())
    }
}

mod backup;
pub mod config;
pub mod guard;
pub mod observe;
pub use backup::{
    check as check_backup, list_backups, prune as prune_backups, restore as restore_backup, stamp, BackupInfo,
};
pub use guard::{Guard, Rate};
pub use observe::{Metrics, Stored};

// ---- schema versions ---------------------------------------------------------

fn has_column(db: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    db.prepare("SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2")?
        .exists([table, column])
}

/// Ordered schema changes. Each runs once, in a transaction, and is written to
/// be harmless on a database that already has its result (relays from before
/// versions were recorded start at the first). Add new ones at the end; never
/// edit or reorder an existing one.
const MIGRATIONS: &[fn(&Connection) -> rusqlite::Result<()>] = &[
    // 1: the original tables.
    |db| {
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS groups (
                id TEXT PRIMARY KEY, token_hash TEXT NOT NULL, created INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS ops (
                group_id TEXT NOT NULL, seq INTEGER NOT NULL, device TEXT NOT NULL,
                blob TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY (group_id, seq)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS devices (
                group_id TEXT NOT NULL, device TEXT NOT NULL, blob TEXT NOT NULL,
                at INTEGER NOT NULL, PRIMARY KEY (group_id, device)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS accounts (
                name TEXT PRIMARY KEY, key_hash TEXT NOT NULL UNIQUE, created INTEGER NOT NULL,
                max_groups INTEGER);",
        )
    },
    // 2: groups belong to an account.
    |db| {
        if !has_column(db, "groups", "account")? {
            db.execute_batch("ALTER TABLE groups ADD COLUMN account TEXT")?;
        }
        Ok(())
    },
    // 3: per-group byte count and last write, kept up to date by every push
    // (counting them on demand would scan a whole group's batches).
    |db| {
        if !has_column(db, "groups", "bytes")? {
            db.execute_batch(
                "ALTER TABLE groups ADD COLUMN bytes INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE groups ADD COLUMN updated INTEGER NOT NULL DEFAULT 0;
                 UPDATE groups SET
                    bytes = COALESCE((SELECT SUM(LENGTH(CAST(blob AS BLOB))) FROM ops WHERE group_id = groups.id), 0),
                    updated = COALESCE((SELECT MAX(at) FROM ops WHERE group_id = groups.id), created);",
            )?;
        }
        Ok(())
    },
    // 4: an epoch naming this database's history. A restored backup gets a new
    // one, so clients notice the relay went back in time.
    |db| {
        db.execute_batch("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")?;
        if !db.prepare("SELECT 1 FROM meta WHERE key = 'epoch'")?.exists([])? {
            set_new_epoch(db)?;
        }
        Ok(())
    },
];

/// The schema version a database written by this relay has.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Give the database a new random epoch (see [`EPOCH_HEADER`]).
pub fn set_new_epoch(db: &Connection) -> rusqlite::Result<()> {
    let mut raw = [0u8; 12];
    getrandom::getrandom(&mut raw)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(std::io::Error::other(e.to_string()).into()))?;
    let epoch: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('epoch', ?1)", [epoch])?;
    Ok(())
}

fn migrate(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL)")?;
    let current: i64 = db.query_row("SELECT COALESCE(MAX(version), 0) FROM schema_version", [], |r| r.get(0))?;
    if current > SCHEMA_VERSION {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_SCHEMA),
            Some(format!(
                "this database is from a newer relay (schema {current}; this one knows {SCHEMA_VERSION}): upgrade the relay"
            )),
        ));
    }
    for (i, step) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = db.unchecked_transaction()?;
        step(&tx)?;
        tx.execute("DELETE FROM schema_version", [])?;
        tx.execute("INSERT INTO schema_version (version) VALUES (?1)", [i as i64 + 1])?;
        tx.commit()?;
    }
    Ok(())
}

// ---- HTTP --------------------------------------------------------------------

fn token(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_string()
}

fn account(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-linecard-account")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

#[derive(Deserialize)]
struct PushBody {
    device: String,
    blob: String,
}

#[derive(Deserialize)]
struct PullQuery {
    #[serde(default)]
    after: i64,
}

#[derive(Deserialize)]
struct WaitQuery {
    #[serde(default)]
    after: i64,
    /// Seconds (capped at MAX_WAIT).
    #[serde(default = "default_wait")]
    wait: u64,
}

fn default_wait() -> u64 {
    MAX_WAIT.as_secs()
}

#[derive(Deserialize)]
struct DeviceBody {
    blob: String,
}

pub(crate) type Shared = Arc<Relay>;

async fn push(
    State(r): State<Shared>,
    UrlPath(group): UrlPath<String>,
    headers: HeaderMap,
    Json(b): Json<PushBody>,
) -> Result<Json<serde_json::Value>> {
    let seq = r.push(
        &group,
        &token(&headers),
        account(&headers).as_deref(),
        &b.device,
        &b.blob,
    )?;
    Ok(Json(serde_json::json!({ "seq": seq })))
}

async fn pull(
    State(r): State<Shared>,
    UrlPath(group): UrlPath<String>,
    Query(q): Query<PullQuery>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let (ops, more) = r.pull(&group, &token(&headers), q.after)?;
    Ok(Json(serde_json::json!({ "ops": ops, "more": more })))
}

async fn wait(
    State(r): State<Shared>,
    UrlPath(group): UrlPath<String>,
    Query(q): Query<WaitQuery>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let latest = r
        .wait(&group, &token(&headers), q.after, Duration::from_secs(q.wait))
        .await?;
    Ok(Json(serde_json::json!({ "latest": latest })))
}

async fn put_device(
    State(r): State<Shared>,
    UrlPath((group, device)): UrlPath<(String, String)>,
    headers: HeaderMap,
    Json(b): Json<DeviceBody>,
) -> Result<StatusCode> {
    r.put_device(&group, &token(&headers), account(&headers).as_deref(), &device, &b.blob)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn devices(
    State(r): State<Shared>,
    UrlPath(group): UrlPath<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let devices = r.devices(&group, &token(&headers))?;
    Ok(Json(serde_json::json!({ "devices": devices })))
}

/// Every response says which database it came from. Restoring a backup
/// gives the relay a new epoch, which tells clients that its history went
/// back in time and they must send theirs again.
async fn add_epoch(State(r): State<Shared>, req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let mut res = next.run(req).await;
    if let Ok(v) = r.epoch().parse() {
        res.headers_mut().insert(EPOCH_HEADER, v);
    }
    res
}

async fn ready(State(r): State<Shared>) -> Response {
    match r.ready() {
        Ok(()) => "ready".into_response(),
        Err(why) => (StatusCode::SERVICE_UNAVAILABLE, why).into_response(),
    }
}

pub fn router(relay: Shared) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ready", get(ready))
        .route("/v1/groups/{group}/ops", get(pull).post(push))
        .route("/v1/groups/{group}/ops/wait", get(wait))
        .route("/v1/groups/{group}/devices", get(devices))
        .route("/v1/groups/{group}/devices/{device}", put(put_device))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BLOB + 4096))
        .layer(axum::middleware::from_fn_with_state(relay.clone(), add_epoch))
        .layer(axum::middleware::from_fn_with_state(relay.clone(), guard::guard))
        .layer(axum::middleware::from_fn_with_state(relay.clone(), observe::observe))
        .with_state(relay)
}

/// `/metrics` (Prometheus), meant for a separate listener that isn't public.
pub fn metrics_router(relay: Shared) -> Router {
    async fn metrics(State(r): State<Shared>) -> Response {
        match r.stored() {
            Ok(stored) => (
                [("content-type", "text/plain; version=0.0.4")],
                r.metrics().render(&stored),
            )
                .into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:?}")).into_response(),
        }
    }
    Router::new().route("/metrics", get(metrics)).with_state(relay)
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: &str = "0123456789abcdef0123456789abcdef";
    const T: &str = "token-for-the-group-xyz";

    fn open_relay(max_ops: i64) -> Relay {
        Relay::in_memory(Limits {
            max_ops,
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn first_writer_claims_the_group() {
        let r = open_relay(100);
        assert_eq!(r.pull(G, T, 0).unwrap().0, vec![], "empty before anyone writes");
        assert_eq!(r.push(G, T, None, "laptop", "b1").unwrap(), 1);
        assert_eq!(r.push(G, T, None, "desktop", "b2").unwrap(), 2);
        assert_eq!(
            r.push(G, "someone-elses-token", None, "x", "evil"),
            Err(RelayError::Unauthorized)
        );
        assert_eq!(
            r.pull(G, "someone-elses-token", 0).unwrap_err(),
            RelayError::Unauthorized
        );
        let (ops, more) = r.pull(G, T, 1).unwrap();
        assert_eq!((ops.len(), ops[0].blob.as_str(), more), (1, "b2", false));
    }

    #[test]
    fn validation_and_limits() {
        let r = open_relay(2);
        assert_eq!(
            r.push("not-hex", T, None, "d", "b"),
            Err(RelayError::BadRequest("bad group id"))
        );
        assert_eq!(
            r.push(G, T, None, "../etc", "b"),
            Err(RelayError::BadRequest("bad device id"))
        );
        assert_eq!(r.push(G, "short", None, "d", "b"), Err(RelayError::Unauthorized));
        assert_eq!(
            r.push(G, T, None, "d", &"x".repeat(MAX_BLOB + 1)),
            Err(RelayError::TooLarge)
        );
        r.push(G, T, None, "d", "1").unwrap();
        r.push(G, T, None, "d", "2").unwrap();
        assert_eq!(r.push(G, T, None, "d", "3"), Err(RelayError::Full));
    }

    #[test]
    fn heartbeats_replace_per_device() {
        let r = open_relay(100);
        r.put_device(G, T, None, "laptop", "v1").unwrap();
        r.put_device(G, T, None, "laptop", "v2").unwrap();
        r.put_device(G, T, None, "desktop", "d1").unwrap();
        let d = r.devices(G, T).unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!(d.iter().find(|x| x.device == "laptop").unwrap().blob, "v2");
    }

    #[test]
    fn paging() {
        let r = open_relay(10_000);
        for i in 0..(PAGE + 5) {
            r.push(G, T, None, "d", &i.to_string()).unwrap();
        }
        let (first, more) = r.pull(G, T, 0).unwrap();
        assert_eq!((first.len() as i64, more), (PAGE, true));
        let (rest, more) = r.pull(G, T, first.last().unwrap().seq).unwrap();
        assert_eq!((rest.len(), more), (5, false));
    }

    #[test]
    fn survives_a_restart() {
        let path = std::env::temp_dir().join(format!("linecard-relay-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let r = Relay::open(&path, Limits::default()).unwrap();
            r.push(G, T, None, "d", "kept").unwrap();
        }
        let r = Relay::open(&path, Limits::default()).unwrap();
        assert_eq!(r.pull(G, T, 0).unwrap().0[0].blob, "kept");
        assert_eq!(
            r.push(G, "other-token-entirely", None, "d", "x"),
            Err(RelayError::Unauthorized)
        );
    }

    fn temp_db(label: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("linecard-relay-{label}-{}.db", std::process::id()));
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", p.display()));
        }
        p
    }

    fn remove_db(p: &std::path::Path) {
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", p.display()));
        }
    }

    #[test]
    fn a_relay_from_before_versions_is_upgraded_in_place() {
        // The schema and data an old relay left behind: no owner column, no byte counts.
        let path = temp_db("legacy");
        {
            let db = Connection::open(&path).unwrap();
            db.execute_batch(
                "CREATE TABLE groups (id TEXT PRIMARY KEY, token_hash TEXT NOT NULL, created INTEGER NOT NULL);
                 CREATE TABLE ops (group_id TEXT NOT NULL, seq INTEGER NOT NULL, device TEXT NOT NULL,
                    blob TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY (group_id, seq)) WITHOUT ROWID;
                 CREATE TABLE devices (group_id TEXT NOT NULL, device TEXT NOT NULL, blob TEXT NOT NULL,
                    at INTEGER NOT NULL, PRIMARY KEY (group_id, device)) WITHOUT ROWID;
                 CREATE TABLE accounts (name TEXT PRIMARY KEY, key_hash TEXT NOT NULL UNIQUE,
                    created INTEGER NOT NULL, max_groups INTEGER);",
            )
            .unwrap();
            db.execute("INSERT INTO groups VALUES (?1, 'h', 1000)", [G]).unwrap();
            db.execute("INSERT INTO ops VALUES (?1, 1, 'd', 'aaaa', 5000)", [G])
                .unwrap();
            db.execute("INSERT INTO ops VALUES (?1, 2, 'd', 'bbbbbb', 7000)", [G])
                .unwrap();
        }
        let r = Relay::open(&path, Limits::default()).unwrap();
        let g = &r.groups().unwrap()[0];
        assert_eq!((g.bytes, g.last, g.ops), (10, Some(7000), 2));
        let version: i64 = r
            .db()
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |x| x.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        drop(r);
        // Opening again changes nothing.
        let r = Relay::open(&path, Limits::default()).unwrap();
        assert_eq!(r.groups().unwrap()[0].bytes, 10);
        drop(r);

        // A database from a newer relay is refused rather than damaged.
        {
            let db = Connection::open(&path).unwrap();
            db.execute("UPDATE schema_version SET version = ?1", [SCHEMA_VERSION + 1])
                .unwrap();
        }
        let err = Relay::open(&path, Limits::default())
            .err()
            .expect("a newer schema must be refused");
        assert!(err.to_string().contains("newer relay"), "{err}");
        remove_db(&path);
    }

    #[test]
    fn groups_stop_at_their_byte_quota() {
        let r = Relay::in_memory(Limits {
            max_group_bytes: 100,
            ..Default::default()
        })
        .unwrap();
        let blob = |n: usize| "x".repeat(n);
        r.push(G, T, None, "d", &blob(60)).unwrap();
        assert_eq!(
            r.push(G, T, None, "d", &blob(41)),
            Err(RelayError::Full),
            "60 + 41 > 100"
        );
        r.push(G, T, None, "d", &blob(40)).unwrap(); // exactly full is allowed
        assert_eq!(r.push(G, T, None, "d", &blob(1)), Err(RelayError::Full));
        assert_eq!(r.groups().unwrap()[0].bytes, 100);
        // Another group has its own allowance, and reading is never blocked.
        r.push("fedcba9876543210fedcba9876543210", T, None, "d", &blob(100))
            .unwrap();
        assert_eq!(r.pull(G, T, 0).unwrap().0.len(), 2);

        // 0 = no limit.
        let open = Relay::in_memory(Limits {
            max_group_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        open.push(G, T, None, "d", &blob(MAX_BLOB)).unwrap();
        open.push(G, T, None, "d", &blob(MAX_BLOB)).unwrap();
    }

    #[test]
    fn maintenance_removes_idle_groups_and_stale_devices_only() {
        let r = Relay::in_memory(Limits {
            idle_days: 30,
            ..Default::default()
        })
        .unwrap();
        let (quiet, busy, heartbeat_only) = (
            "00000000000000000000000000000001",
            "00000000000000000000000000000002",
            "00000000000000000000000000000003",
        );
        for g in [quiet, busy, heartbeat_only] {
            r.push(g, T, None, "d", "blob").unwrap();
        }
        r.put_device(heartbeat_only, T, None, "laptop", "hb").unwrap();
        r.put_device(busy, T, None, "old-pc", "hb").unwrap();
        let ago = |days: i64| now_ms() - days * DAY_MS;
        {
            // `quiet`: nothing for 40 days. `heartbeat_only`: last write 40 days
            // ago, but a computer still reports in. `busy`: written today, with
            // one computer unseen for 100 days.
            let db = r.db().unwrap();
            for g in [quiet, heartbeat_only] {
                db.execute(
                    "UPDATE groups SET updated = ?2, created = ?2 WHERE id = ?1",
                    params![g, ago(40)],
                )
                .unwrap();
            }
            db.execute("UPDATE devices SET at = ?1 WHERE device = 'old-pc'", [ago(100)])
                .unwrap();
        }
        let idle: Vec<String> = r.idle_groups(30).unwrap().into_iter().map(|g| g.id).collect();
        assert_eq!(idle, [quiet]);

        let m = r.maintain().unwrap();
        assert_eq!(
            m,
            Maintenance {
                expired: vec![quiet.to_string()],
                devices_removed: 1
            }
        );
        let mut left: Vec<String> = r.groups().unwrap().into_iter().map(|g| g.id).collect();
        left.sort();
        assert_eq!(left, [busy, heartbeat_only]);
        assert!(r.devices(busy, T).unwrap().is_empty(), "the stale heartbeat is gone");
        assert_eq!(r.devices(heartbeat_only, T).unwrap().len(), 1);
        assert!(r.pull(quiet, T, 0).unwrap().0.is_empty(), "its batches went with it");
    }

    #[test]
    fn nothing_expires_unless_asked_to() {
        let r = open_relay(100); // idle_days = 0
        r.push(G, T, None, "d", "blob").unwrap();
        r.db()
            .unwrap()
            .execute("UPDATE groups SET updated = 1, created = 1", [])
            .unwrap();
        assert_eq!(r.maintain().unwrap(), Maintenance::default());
        assert_eq!(r.groups().unwrap().len(), 1);
    }

    #[test]
    fn the_epoch_is_stable_across_restarts_and_new_for_new_databases() {
        let path = temp_db("epoch");
        let first = Relay::open(&path, Limits::default()).unwrap().epoch().to_string();
        assert_eq!(first.len(), 24);
        assert_eq!(Relay::open(&path, Limits::default()).unwrap().epoch(), first);
        remove_db(&path);
        assert_ne!(Relay::open(&path, Limits::default()).unwrap().epoch(), first);
        remove_db(&path);
    }

    #[test]
    fn vacuum_runs_on_a_file_database() {
        let path = temp_db("vacuum");
        let r = Relay::open(&path, Limits::default()).unwrap();
        r.push(G, T, None, "d", &"x".repeat(200_000)).unwrap();
        r.remove_group(G).unwrap();
        assert!(r.vacuum().unwrap() > 0, "deleted data leaves free pages");
        drop(r);
        remove_db(&path);
    }

    #[test]
    fn private_relays_need_an_account_to_start_groups() {
        let r = Relay::in_memory(Limits {
            max_ops: 100,
            open: false,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(r.push(G, T, None, "d", "x"), Err(RelayError::NeedsAccount));
        assert_eq!(r.push(G, T, Some("lcr_wrong"), "d", "x"), Err(RelayError::NeedsAccount));
        let key = r.add_account("alice", Some(1)).unwrap();
        assert!(r.add_account("alice", None).is_err(), "names are unique");
        r.push(G, T, Some(&key), "laptop", "x").unwrap();
        // Other devices join with the phrase alone.
        r.push(G, T, None, "desktop", "y").unwrap();
        // One group allowed.
        let g2 = "fedcba9876543210fedcba9876543210";
        assert_eq!(r.push(g2, T, Some(&key), "d", "x"), Err(RelayError::Full));

        let accounts = r.accounts().unwrap();
        assert_eq!((accounts[0].name.as_str(), accounts[0].groups), ("alice", 1));
        let groups = r.groups().unwrap();
        assert_eq!((groups[0].account.as_deref(), groups[0].ops), (Some("alice"), 2));

        assert_eq!(r.remove_account("alice", true).unwrap(), 1);
        assert!(r.groups().unwrap().is_empty());
        assert!(r.accounts().unwrap().is_empty());
    }

    #[tokio::test]
    async fn waiters_wake_when_something_arrives() {
        let r = Arc::new(open_relay(100));
        r.push(G, T, None, "d", "first").unwrap();
        // Already newer than the cursor: no waiting.
        assert_eq!(r.wait(G, T, 0, Duration::from_secs(5)).await.unwrap(), 1);

        let waiting = {
            let r = r.clone();
            tokio::spawn(async move {
                let started = std::time::Instant::now();
                let latest = r.wait(G, T, 1, Duration::from_secs(10)).await.unwrap();
                (latest, started.elapsed())
            })
        };
        tokio::time::sleep(Duration::from_millis(150)).await;
        r.push(G, T, None, "d", "second").unwrap();
        let (latest, took) = waiting.await.unwrap();
        assert_eq!(latest, 2);
        assert!(
            took < Duration::from_secs(2),
            "woke on the push, not the timeout: {took:?}"
        );

        // Nothing arrives: back after the wait.
        assert_eq!(r.wait(G, T, 2, Duration::from_millis(100)).await.unwrap(), 2);
    }
}
