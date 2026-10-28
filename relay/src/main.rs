//! Linecard relay server. See lib.rs for the protocol.
//!
//! Environment:
//!   LINECARD_RELAY_ADDR      listen address (default 0.0.0.0:8443)
//!   LINECARD_RELAY_DB        SQLite file (default ./linecard-relay.db)
//!   LINECARD_RELAY_MAX_OPS   batches kept per sync group (default 200000)
//!
//! Serve it behind HTTPS (a reverse proxy such as Caddy or Traefik); the
//! data is end-to-end encrypted, but the token travels in a header.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use linecard_relay::{router, Relay};

#[tokio::main]
async fn main() {
    let addr: SocketAddr = std::env::var("LINECARD_RELAY_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8443".into())
        .parse()
        .expect("LINECARD_RELAY_ADDR must be host:port");
    let db = PathBuf::from(std::env::var("LINECARD_RELAY_DB").unwrap_or_else(|_| "linecard-relay.db".into()));
    let max_ops = std::env::var("LINECARD_RELAY_MAX_OPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200_000);
    let relay = Relay::open(&db, max_ops).unwrap_or_else(|e| panic!("can't open {}: {e}", db.display()));
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    println!("linecard-relay listening on {addr}, data in {}", db.display());
    axum::serve(listener, router(Arc::new(relay))).await.expect("server");
}
