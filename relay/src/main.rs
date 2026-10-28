//! Linecard relay (skeleton).
//!
//! The relay never sees plaintext: clients upload opaque, encrypted op
//! batches tagged with a sync-group id, and other devices in the group fetch
//! everything after their cursor. Phase 2 adds: user accounts and API tokens
//! (multi-user from day one), WebSocket push, per-user/per-group quotas,
//! SQLite persistence, snapshots, and an admin CLI.
//!
//! Endpoints today (in-memory, single shared token from LINECARD_RELAY_TOKEN):
//!   GET  /health
//!   POST /v1/groups/{group}/ops            body: {"device": "...", "blob": "<base64 ciphertext>"}
//!   GET  /v1/groups/{group}/ops?after=N    -> {"ops": [{"seq": N, "device": "...", "blob": "..."}]}

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

#[derive(Clone, Serialize)]
struct StoredOp {
    seq: u64,
    device: String,
    blob: String,
}

#[derive(Default)]
struct Groups {
    ops: HashMap<String, Vec<StoredOp>>,
}

#[derive(Clone)]
struct AppState {
    token: Arc<String>,
    groups: Arc<RwLock<Groups>>,
}

#[derive(Deserialize)]
struct PushBody {
    device: String,
    blob: String,
}

#[derive(Deserialize)]
struct PullQuery {
    #[serde(default)]
    after: u64,
}

const MAX_BLOB_BYTES: usize = 1 << 20;

fn authorized(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
    let expected = format!("Bearer {}", state.token);
    match headers.get("authorization").and_then(|v| v.to_str().ok()) {
        Some(v) if v == expected => Ok(()),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

async fn push(
    State(state): State<AppState>,
    Path(group): Path<String>,
    headers: HeaderMap,
    Json(body): Json<PushBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    authorized(&state, &headers)?;
    if body.blob.len() > MAX_BLOB_BYTES {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let mut groups = state.groups.write().await;
    let log = groups.ops.entry(group).or_default();
    let seq = log.len() as u64 + 1;
    log.push(StoredOp {
        seq,
        device: body.device,
        blob: body.blob,
    });
    Ok(Json(serde_json::json!({ "seq": seq })))
}

async fn pull(
    State(state): State<AppState>,
    Path(group): Path<String>,
    Query(q): Query<PullQuery>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    authorized(&state, &headers)?;
    let groups = state.groups.read().await;
    let ops: Vec<StoredOp> = groups
        .ops
        .get(&group)
        .map(|log| log.iter().filter(|o| o.seq > q.after).cloned().collect())
        .unwrap_or_default();
    Ok(Json(serde_json::json!({ "ops": ops })))
}

fn app(token: String) -> Router {
    let state = AppState {
        token: Arc::new(token),
        groups: Arc::default(),
    };
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/v1/groups/{group}/ops", get(pull).post(push))
        .with_state(state)
}

#[tokio::main]
async fn main() {
    let token = std::env::var("LINECARD_RELAY_TOKEN").expect("set LINECARD_RELAY_TOKEN");
    let addr: SocketAddr = std::env::var("LINECARD_RELAY_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8443".into())
        .parse()
        .expect("LINECARD_RELAY_ADDR must be host:port");
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    println!("linecard-relay listening on {addr}");
    axum::serve(listener, app(token)).await.expect("server");
}
