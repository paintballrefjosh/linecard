//! Sync directly between computers on the same network, no server needed.
//!
//! Each computer keeps a store of every sealed item it has (its own and what
//! it heard from others) and serves it with the relay protocol on a random
//! port, behind the group's token. Computers find each other with a
//! multicast beacon every few seconds; the beacon carries a hash of the
//! group id and the port, never the key. Each one pulls from every peer it
//! hears and keeps what it learns, so changes travel A → B → C even if A and
//! C are never on at the same time.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use linecard_relay::{router, Limits, Relay};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::relay::RelayTransport;
use crate::{Received, Sealed, SyncError};

/// UDP port for beacons (multicast, so any number of listeners per host).
pub const LAN_PORT: u16 = 47123;
const MULTICAST: Ipv4Addr = Ipv4Addr::new(239, 255, 42, 99);
const BEACON_EVERY: Duration = Duration::from_secs(3);
/// A peer not heard from for this long is left out.
const PEER_TTL: Duration = Duration::from_secs(15);

pub type LanCursor = BTreeMap<String, i64>;

#[derive(Serialize, Deserialize)]
struct Beacon {
    linecard: u32,
    /// Hash of the group id, so unrelated groups ignore each other.
    group: String,
    device: String,
    port: u16,
}

#[derive(Clone)]
struct Peer {
    addr: SocketAddr,
    seen: Instant,
}

pub struct LanTransport {
    group_id: String,
    token: String,
    device: String,
    store: Arc<Relay>,
    port: u16,
    peers: Arc<Mutex<HashMap<String, Peer>>>,
    /// Nonces of items already stored (each sealed item has a unique one).
    seen: Mutex<HashSet<String>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for LanTransport {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

fn group_tag(group_id: &str) -> String {
    Sha256::digest(format!("linecard-lan:{group_id}").as_bytes())[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn store_err(e: impl std::fmt::Debug) -> SyncError {
    SyncError::Storage(format!("{e:?}"))
}

impl LanTransport {
    /// Start serving and (with `discover`) announcing on the network.
    /// `store` persists what this computer has; `None` keeps it in memory.
    pub async fn start(
        store: Option<&Path>,
        group_id: &str,
        token: &str,
        device: &str,
        discover: bool,
    ) -> Result<Self, SyncError> {
        let limits = Limits {
            max_ops: 5_000_000,
            open: true,
        };
        let store = Arc::new(
            match store {
                Some(p) => Relay::open(p, limits),
                None => Relay::in_memory(limits),
            }
            .map_err(store_err)?,
        );

        // What's already stored.
        let mut seen = HashSet::new();
        let mut after = 0;
        loop {
            let (ops, more) = store.pull(group_id, token, after).map_err(store_err)?;
            for op in &ops {
                if let Ok(s) = serde_json::from_str::<Sealed>(&op.blob) {
                    seen.insert(s.nonce);
                }
                after = op.seq;
            }
            if !more {
                break;
            }
        }

        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await?;
        let port = listener.local_addr()?.port();
        let mut tasks = vec![tokio::spawn({
            let app = router(store.clone());
            async move {
                let _ = axum::serve(listener, app).await;
            }
        })];

        let peers: Arc<Mutex<HashMap<String, Peer>>> = Default::default();
        if discover {
            tasks.extend(discovery(group_id, device, port, peers.clone())?);
        }
        Ok(LanTransport {
            group_id: group_id.to_string(),
            token: token.to_string(),
            device: device.to_string(),
            store,
            port,
            peers,
            seen: Mutex::new(seen),
            tasks,
        })
    }

    /// The port this computer serves on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Add a peer by hand (tests, or networks without multicast).
    pub fn add_peer(&self, device: &str, addr: SocketAddr) {
        if let Ok(mut p) = self.peers.lock() {
            p.insert(
                device.to_string(),
                Peer {
                    addr,
                    seen: Instant::now() + Duration::from_secs(3600),
                },
            );
        }
    }

    /// Computers heard from recently.
    pub fn peers(&self) -> Vec<(String, SocketAddr)> {
        let now = Instant::now();
        self.peers
            .lock()
            .map(|p| {
                p.iter()
                    .filter(|(_, peer)| peer.seen + PEER_TTL > now || peer.seen > now)
                    .map(|(d, peer)| (d.clone(), peer.addr))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Keep an item (and serve it to others), once.
    fn keep(&self, sealed: &Sealed) -> Result<bool, SyncError> {
        let fresh = self
            .seen
            .lock()
            .map(|mut s| s.insert(sealed.nonce.clone()))
            .unwrap_or(false);
        if fresh {
            let blob = serde_json::to_string(sealed).map_err(|e| SyncError::Storage(e.to_string()))?;
            self.store
                .push(&self.group_id, &self.token, None, &sealed.device, &blob)
                .map_err(store_err)?;
        }
        Ok(fresh)
    }

    pub fn push(&self, sealed: &Sealed) -> Result<(), SyncError> {
        self.keep(sealed).map(|_| ())
    }

    /// New items from every peer in reach; the cursor is per peer. A peer
    /// that has gone away is skipped, not an error.
    pub async fn pull(&self, cursor: &LanCursor) -> Result<(Vec<Received>, LanCursor), SyncError> {
        let mut out = Vec::new();
        let mut next = cursor.clone();
        for (peer, addr) in self.peers() {
            let client = RelayTransport::new(&format!("http://{addr}"), &self.group_id, &self.token, &self.device)?;
            let Ok((items, last)) = client.pull(cursor.get(&peer).copied().unwrap_or(0)).await else {
                continue;
            };
            for item in items {
                if self.keep(&item.sealed)? && item.sealed.device != self.device {
                    out.push(item);
                }
            }
            next.insert(peer, last);
        }
        Ok((out, next))
    }

    pub fn put_device(&self, sealed: &Sealed) -> Result<(), SyncError> {
        let blob = serde_json::to_string(sealed).map_err(|e| SyncError::Storage(e.to_string()))?;
        self.store
            .put_device(&self.group_id, &self.token, None, &self.device, &blob)
            .map_err(store_err)
    }

    /// Heartbeats from every peer in reach (the newest per device).
    pub async fn devices(&self) -> Result<Vec<Received>, SyncError> {
        let mut newest: HashMap<String, Received> = HashMap::new();
        for (_, addr) in self.peers() {
            let client = RelayTransport::new(&format!("http://{addr}"), &self.group_id, &self.token, &self.device)?;
            for r in client.devices().await.unwrap_or_default() {
                let keep = newest
                    .get(&r.sealed.device)
                    .is_none_or(|old| r.stored_at > old.stored_at);
                if keep {
                    newest.insert(r.sealed.device.clone(), r);
                }
            }
        }
        Ok(newest.into_values().collect())
    }
}

/// Beacon sender and listener.
fn discovery(
    group_id: &str,
    device: &str,
    port: u16,
    peers: Arc<Mutex<HashMap<String, Peer>>>,
) -> Result<Vec<tokio::task::JoinHandle<()>>, SyncError> {
    use socket2::{Domain, Protocol, Socket, Type};

    let tag = group_tag(group_id);
    let listen = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    listen.set_reuse_address(true)?;
    listen.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, LAN_PORT).into())?;
    listen.join_multicast_v4(&MULTICAST, &Ipv4Addr::UNSPECIFIED)?;
    listen.set_nonblocking(true)?;
    let listen = tokio::net::UdpSocket::from_std(listen.into())?;

    let send = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    send.set_multicast_loop_v4(true)?;
    send.set_nonblocking(true)?;
    send.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0).into())?;
    let send = tokio::net::UdpSocket::from_std(send.into())?;

    let me = device.to_string();
    let beacon = serde_json::to_vec(&Beacon {
        linecard: 1,
        group: tag.clone(),
        device: device.to_string(),
        port,
    })
    .map_err(|e| SyncError::Storage(e.to_string()))?;

    let announcer = tokio::spawn(async move {
        loop {
            let _ = send.send_to(&beacon, (MULTICAST, LAN_PORT)).await;
            tokio::time::sleep(BEACON_EVERY).await;
        }
    });
    let listener = tokio::spawn(async move {
        let mut buf = [0u8; 512];
        loop {
            let Ok((n, from)) = listen.recv_from(&mut buf).await else {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            };
            let Ok(b) = serde_json::from_slice::<Beacon>(&buf[..n]) else {
                continue;
            };
            if b.linecard != 1 || b.group != tag || b.device == me {
                continue;
            }
            if let Ok(mut p) = peers.lock() {
                p.insert(
                    b.device,
                    Peer {
                        addr: SocketAddr::new(from.ip(), b.port),
                        seen: Instant::now(),
                    },
                );
            }
        }
    });
    Ok(vec![announcer, listener])
}
