//! Linecard relay: stores and forwards end-to-end encrypted sync data.
//!
//! The relay never sees plaintext. A sync group is known only by a group id
//! derived from its key; clients prove membership with a bearer token, also
//! derived from the key, which the relay stores only as a SHA-256 hash. The
//! first device to write to a group claims it with its token; after that,
//! requests without the same token are refused. No accounts are needed: the
//! 24-word phrase is the credential.
//!
//! ```text
//! GET  /health
//! POST /v1/groups/{group}/ops                {device, blob}   -> {seq}
//! GET  /v1/groups/{group}/ops?after=N        -> {ops: [{seq, device, blob, at}], more}
//! PUT  /v1/groups/{group}/devices/{device}   {blob}           heartbeat
//! GET  /v1/groups/{group}/devices            -> {devices: [{device, blob, at}]}
//! ```

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Largest single blob (one sealed batch or heartbeat).
pub const MAX_BLOB: usize = 1 << 20;
/// Batches returned per pull; clients ask again while `more` is true.
const PAGE: i64 = 500;

#[derive(Debug, PartialEq, Eq)]
pub enum RelayError {
    BadRequest(&'static str),
    Unauthorized,
    TooLarge,
    Full,
    Internal(String),
}

impl IntoResponse for RelayError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            RelayError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.to_string()),
            RelayError::Unauthorized => (StatusCode::UNAUTHORIZED, "wrong token for this group".into()),
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

pub struct Relay {
    db: Mutex<Connection>,
    /// Batches kept per group before writes are refused.
    max_ops: i64,
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

fn valid_group(g: &str) -> bool {
    g.len() == 32 && g.bytes().all(|b| b.is_ascii_hexdigit())
}

fn valid_device(d: &str) -> bool {
    !d.is_empty() && d.len() <= 64 && d.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Relay {
    pub fn open(path: &Path, max_ops: i64) -> rusqlite::Result<Self> {
        Self::init(Connection::open(path)?, max_ops)
    }

    pub fn in_memory(max_ops: i64) -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?, max_ops)
    }

    fn init(db: Connection, max_ops: i64) -> rusqlite::Result<Self> {
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS groups (
                id TEXT PRIMARY KEY, token_hash TEXT NOT NULL, created INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS ops (
                group_id TEXT NOT NULL, seq INTEGER NOT NULL, device TEXT NOT NULL,
                blob TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY (group_id, seq)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS devices (
                group_id TEXT NOT NULL, device TEXT NOT NULL, blob TEXT NOT NULL,
                at INTEGER NOT NULL, PRIMARY KEY (group_id, device)) WITHOUT ROWID;",
        )?;
        Ok(Relay {
            db: Mutex::new(db),
            max_ops,
        })
    }

    fn db(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.db
            .lock()
            .map_err(|_| RelayError::Internal("database lock poisoned".into()))
    }

    /// Check the token; the first writer of a new group claims it.
    fn authorize(db: &Connection, group: &str, token: &str, may_claim: bool) -> Result<()> {
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
                db.execute(
                    "INSERT INTO groups (id, token_hash, created) VALUES (?1, ?2, ?3)",
                    params![group, hash(token), now_ms()],
                )?;
                Ok(())
            }
            // Reading a group nobody has written to yet: nothing to see.
            None => Ok(()),
        }
    }

    pub fn push(&self, group: &str, token: &str, device: &str, blob: &str) -> Result<i64> {
        if !valid_device(device) {
            return Err(RelayError::BadRequest("bad device id"));
        }
        if blob.len() > MAX_BLOB {
            return Err(RelayError::TooLarge);
        }
        let mut db = self.db()?;
        Self::authorize(&db, group, token, true)?;
        let tx = db.transaction()?;
        let (count, last): (i64, Option<i64>) =
            tx.query_row("SELECT COUNT(*), MAX(seq) FROM ops WHERE group_id = ?1", [group], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        if count >= self.max_ops {
            return Err(RelayError::Full);
        }
        let seq = last.unwrap_or(0) + 1;
        tx.execute(
            "INSERT INTO ops (group_id, seq, device, blob, at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![group, seq, device, blob, now_ms()],
        )?;
        tx.commit()?;
        Ok(seq)
    }

    /// Batches after `after`, oldest first; `true` if there are more.
    pub fn pull(&self, group: &str, token: &str, after: i64) -> Result<(Vec<StoredOp>, bool)> {
        let db = self.db()?;
        Self::authorize(&db, group, token, false)?;
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

    pub fn put_device(&self, group: &str, token: &str, device: &str, blob: &str) -> Result<()> {
        if !valid_device(device) {
            return Err(RelayError::BadRequest("bad device id"));
        }
        if blob.len() > MAX_BLOB {
            return Err(RelayError::TooLarge);
        }
        let db = self.db()?;
        Self::authorize(&db, group, token, true)?;
        db.execute(
            "INSERT INTO devices (group_id, device, blob, at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(group_id, device) DO UPDATE SET blob = excluded.blob, at = excluded.at",
            params![group, device, blob, now_ms()],
        )?;
        Ok(())
    }

    pub fn devices(&self, group: &str, token: &str) -> Result<Vec<StoredDevice>> {
        let db = self.db()?;
        Self::authorize(&db, group, token, false)?;
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
struct DeviceBody {
    blob: String,
}

type Shared = Arc<Relay>;

async fn push(
    State(r): State<Shared>,
    UrlPath(group): UrlPath<String>,
    headers: HeaderMap,
    Json(b): Json<PushBody>,
) -> Result<Json<serde_json::Value>> {
    let seq = r.push(&group, &token(&headers), &b.device, &b.blob)?;
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

async fn put_device(
    State(r): State<Shared>,
    UrlPath((group, device)): UrlPath<(String, String)>,
    headers: HeaderMap,
    Json(b): Json<DeviceBody>,
) -> Result<StatusCode> {
    r.put_device(&group, &token(&headers), &device, &b.blob)?;
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

pub fn router(relay: Shared) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/v1/groups/{group}/ops", get(pull).post(push))
        .route("/v1/groups/{group}/devices", get(devices))
        .route("/v1/groups/{group}/devices/{device}", put(put_device))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BLOB + 4096))
        .with_state(relay)
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: &str = "0123456789abcdef0123456789abcdef";
    const T: &str = "token-for-the-group-xyz";

    #[test]
    fn first_writer_claims_the_group() {
        let r = Relay::in_memory(100).unwrap();
        assert_eq!(r.pull(G, T, 0).unwrap().0, vec![], "empty before anyone writes");
        assert_eq!(r.push(G, T, "laptop", "b1").unwrap(), 1);
        assert_eq!(r.push(G, T, "desktop", "b2").unwrap(), 2);
        assert_eq!(
            r.push(G, "someone-elses-token", "x", "evil"),
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
        let r = Relay::in_memory(2).unwrap();
        assert_eq!(
            r.push("not-hex", T, "d", "b"),
            Err(RelayError::BadRequest("bad group id"))
        );
        assert_eq!(
            r.push(G, T, "../etc", "b"),
            Err(RelayError::BadRequest("bad device id"))
        );
        assert_eq!(r.push(G, "short", "d", "b"), Err(RelayError::Unauthorized));
        assert_eq!(r.push(G, T, "d", &"x".repeat(MAX_BLOB + 1)), Err(RelayError::TooLarge));
        r.push(G, T, "d", "1").unwrap();
        r.push(G, T, "d", "2").unwrap();
        assert_eq!(r.push(G, T, "d", "3"), Err(RelayError::Full));
    }

    #[test]
    fn heartbeats_replace_per_device() {
        let r = Relay::in_memory(100).unwrap();
        r.put_device(G, T, "laptop", "v1").unwrap();
        r.put_device(G, T, "laptop", "v2").unwrap();
        r.put_device(G, T, "desktop", "d1").unwrap();
        let d = r.devices(G, T).unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!(d.iter().find(|x| x.device == "laptop").unwrap().blob, "v2");
    }

    #[test]
    fn paging() {
        let r = Relay::in_memory(10_000).unwrap();
        for i in 0..(PAGE + 5) {
            r.push(G, T, "d", &i.to_string()).unwrap();
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
            let r = Relay::open(&path, 100).unwrap();
            r.push(G, T, "d", "kept").unwrap();
        }
        let r = Relay::open(&path, 100).unwrap();
        assert_eq!(r.pull(G, T, 0).unwrap().0[0].blob, "kept");
        assert_eq!(
            r.push(G, "other-token-entirely", "d", "x"),
            Err(RelayError::Unauthorized)
        );
    }
}
