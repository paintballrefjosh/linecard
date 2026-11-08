//! Sync through a Linecard relay (see `relay/`). The relay stores sealed
//! blobs per group in arrival order; the cursor is the last sequence number
//! seen.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;

use serde::Deserialize;

use crate::{Received, Sealed, SyncError};

pub struct RelayTransport {
    base: String,
    group: String,
    token: String,
    device: String,
    /// Access key for a private relay (needed to start a group there).
    account: Option<String>,
    http: reqwest::Client,
    push: tokio::sync::Mutex<Push>,
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The relay's WebSocket, once connected, and when to try again after a failure.
#[derive(Default)]
struct Push {
    socket: Option<Box<Socket>>,
    retry_at: Option<Instant>,
}

/// After failing to connect (an old relay, a proxy that drops sockets): fall
/// back to long-polling for this long.
const WS_RETRY_CONNECT: Duration = Duration::from_secs(600);
/// After a connected socket breaks.
const WS_RETRY_BROKEN: Duration = Duration::from_secs(20);
/// One wait ends after this long with nothing new, so callers keep checking.
const WS_WAIT: Duration = Duration::from_secs(55);

fn tls_config() -> Result<Arc<rustls::ClientConfig>, SyncError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier =
        rustls_platform_verifier::Verifier::new(provider.clone()).map_err(|e| SyncError::Relay(e.to_string()))?;
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| SyncError::Relay(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok(Arc::new(config))
}
#[derive(Deserialize)]
struct WaitReply {
    latest: i64,
}

#[derive(Deserialize)]
struct OpsPage {
    ops: Vec<StoredOp>,
    more: bool,
}

#[derive(Deserialize)]
struct StoredOp {
    seq: i64,
    blob: String,
    at: i64,
}

#[derive(Deserialize)]
struct DevicesPage {
    devices: Vec<StoredDevice>,
}

#[derive(Deserialize)]
struct StoredDevice {
    blob: String,
    at: i64,
}

fn http_err(e: reqwest::Error) -> SyncError {
    if e.is_timeout() || e.is_connect() {
        SyncError::Relay(format!("can't reach the relay ({e})"))
    } else {
        SyncError::Relay(e.to_string())
    }
}

/// What the relay stores for a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub struct RelayStats {
    pub batches: i64,
    pub bytes: i64,
    pub head: i64,
}

async fn check(resp: reqwest::Response) -> Result<reqwest::Response, SyncError> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    if resp.status().as_u16() == 429 {
        // Obey the relay: it says when there will be room again.
        let secs = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(30);
        return Err(SyncError::RateLimited(secs.clamp(1, 600)));
    }
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    let msg = body["error"].as_str().unwrap_or("").to_string();
    Err(SyncError::Relay(match status.as_u16() {
        401 => "the relay refused this sync phrase (the group belongs to another phrase)".into(),
        403 => "this relay is private: enter the access key its operator gave you".into(),
        507 => "the relay's storage limit for this group is reached".into(),
        _ => format!("{status} {msg}"),
    }))
}

/// The relay's address must use https, except for a relay on this computer
/// or your own network: every request carries the group's token, and over
/// plain http anyone on the path could read it and write to (or compact)
/// the group. The data itself is end-to-end encrypted either way.
fn check_address(base: &str) -> Result<(), SyncError> {
    let url = reqwest::Url::parse(base)
        .map_err(|_| SyncError::Relay("the relay address must look like https://relay.example.com".into()))?;
    match url.scheme() {
        "https" => Ok(()),
        "http" if url.host().is_some_and(|h| is_local(&h)) => Ok(()),
        "http" => Err(SyncError::Relay(
            "use https:// for a relay on the internet (plain http:// is only for this computer or your own network)"
                .into(),
        )),
        _ => Err(SyncError::Relay("the relay address must start with https://".into())),
    }
}

/// This computer, a private or link-local address, or a name that only
/// resolves inside a home or office network.
fn is_local(host: &url::Host<&str>) -> bool {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    let v4 = |ip: Ipv4Addr| {
        let [a, b, ..] = ip.octets();
        // 100.64.0.0/10 is carrier-grade NAT, which Tailscale uses too.
        ip.is_loopback() || ip.is_private() || ip.is_link_local() || (a == 100 && (64..128).contains(&b))
    };
    let v6 = |ip: Ipv6Addr| {
        let first = ip.segments()[0];
        ip.is_loopback()
            || first & 0xfe00 == 0xfc00 // unique local fc00::/7
            || first & 0xffc0 == 0xfe80 // link-local fe80::/10
            || ip.to_ipv4_mapped().is_some_and(v4)
    };
    match host {
        url::Host::Ipv4(ip) => v4(*ip),
        url::Host::Ipv6(ip) => v6(*ip),
        url::Host::Domain(name) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            if let Ok(ip) = name.parse::<IpAddr>() {
                return match ip {
                    IpAddr::V4(ip) => v4(ip),
                    IpAddr::V6(ip) => v6(ip),
                };
            }
            !name.contains('.')
                || [".localhost", ".local", ".lan", ".home.arpa", ".internal"]
                    .iter()
                    .any(|suffix| name.ends_with(suffix))
        }
    }
}

impl RelayTransport {
    /// `base` like `https://relay.example.com`.
    pub fn new(base: &str, group_id: &str, token: &str, device: &str) -> Result<Self, SyncError> {
        let base = base.trim().trim_end_matches('/').to_string();
        check_address(&base)?;
        Ok(RelayTransport {
            base,
            group: group_id.to_string(),
            token: token.to_string(),
            device: device.to_string(),
            account: None,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .map_err(http_err)?,
            push: Default::default(),
        })
    }

    /// Access key for a private relay.
    pub fn with_account(mut self, key: Option<String>) -> Self {
        self.account = key.filter(|k| !k.trim().is_empty()).map(|k| k.trim().to_string());
        self
    }

    fn writing(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.account {
            Some(k) => req.header("x-linecard-account", k),
            None => req,
        }
    }

    /// Wait until the group has something after `cursor`; returns the newest
    /// sequence number. Uses the relay's WebSocket when it has one (the relay
    /// says so the moment a batch arrives), and otherwise holds a request
    /// open for up to ~25 s.
    pub async fn wait(&self, cursor: i64) -> Result<i64, SyncError> {
        if let Some(latest) = self.wait_push(cursor).await {
            return Ok(latest);
        }
        self.wait_poll(cursor).await
    }

    /// `None`: no WebSocket right now (connecting failed, or it just broke).
    async fn wait_push(&self, cursor: i64) -> Option<i64> {
        let mut push = self.push.lock().await;
        if push.retry_at.is_some_and(|t| Instant::now() < t) {
            return None;
        }
        if push.socket.is_none() {
            match self.connect_push().await {
                Ok(s) => {
                    push.socket = Some(Box::new(s));
                    push.retry_at = None;
                }
                Err(_) => {
                    push.retry_at = Some(Instant::now() + WS_RETRY_CONNECT);
                    return None;
                }
            }
        }
        let deadline = Instant::now() + WS_WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let sock = push.socket.as_mut()?;
            match tokio::time::timeout(left, sock.next()).await {
                Err(_) => return Some(cursor), // nothing new; the caller asks again
                Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t)))) => {
                    if let Ok(reply) = serde_json::from_str::<WaitReply>(t.as_str()) {
                        if reply.latest > cursor {
                            return Some(reply.latest);
                        }
                    }
                }
                Ok(Some(Ok(_))) => {} // pings, pongs
                Ok(_) => {
                    push.socket = None;
                    push.retry_at = Some(Instant::now() + WS_RETRY_BROKEN);
                    return None;
                }
            }
        }
    }

    async fn connect_push(&self) -> Result<Socket, SyncError> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let url = self.url("ws").replacen("http", "ws", 1); // http -> ws, https -> wss
        let mut request = url.into_client_request().map_err(|e| SyncError::Relay(e.to_string()))?;
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", self.token)
                .parse()
                .map_err(|_| SyncError::Relay("bad token".into()))?,
        );
        let connector = tokio_tungstenite::Connector::Rustls(tls_config()?);
        let (socket, _) = tokio::time::timeout(
            Duration::from_secs(10),
            tokio_tungstenite::connect_async_tls_with_config(request, None, false, Some(connector)),
        )
        .await
        .map_err(|_| SyncError::Relay("the relay didn't answer the WebSocket in time".into()))?
        .map_err(|e| SyncError::Relay(e.to_string()))?;
        Ok(socket)
    }

    /// Wait (up to ~25 s) until the group has something after `cursor`;
    /// returns the newest sequence number.
    async fn wait_poll(&self, cursor: i64) -> Result<i64, SyncError> {
        let resp = self
            .http
            .get(self.url(&format!("ops/wait?after={cursor}&wait=25")))
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(40))
            .send()
            .await
            .map_err(http_err)?;
        let reply: WaitReply = check(resp).await?.json().await.map_err(http_err)?;
        Ok(reply.latest)
    }

    /// Which database history the relay is serving. It changes when the
    /// operator restores a backup, which sends the relay back in time:
    /// computers must then send their history again. `None` from a relay
    /// that doesn't say (an older one).
    pub async fn epoch(&self) -> Result<Option<String>, SyncError> {
        let resp = self
            .http
            .get(format!("{}/health", self.base))
            .send()
            .await
            .map_err(http_err)?;
        Ok(resp
            .headers()
            .get("x-linecard-epoch")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string))
    }

    fn url(&self, rest: &str) -> String {
        format!("{}/v1/groups/{}/{rest}", self.base, self.group)
    }

    pub async fn push(&self, sealed: &Sealed) -> Result<(), SyncError> {
        let blob = serde_json::to_string(sealed).map_err(|e| SyncError::Storage(e.to_string()))?;
        let resp = self
            .writing(self.http.post(self.url("ops")))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "device": self.device, "blob": blob }))
            .send()
            .await
            .map_err(http_err)?;
        check(resp).await?;
        Ok(())
    }

    /// Everything after `cursor` (including this device's own pushes, which
    /// the replica ignores as duplicates); returns the new cursor.
    pub async fn pull(&self, cursor: i64) -> Result<(Vec<Received>, i64), SyncError> {
        let mut out = Vec::new();
        let mut after = cursor;
        loop {
            let resp = self
                .http
                .get(self.url(&format!("ops?after={after}")))
                .bearer_auth(&self.token)
                .send()
                .await
                .map_err(http_err)?;
            let page: OpsPage = check(resp).await?.json().await.map_err(http_err)?;
            for op in page.ops {
                after = op.seq;
                // A blob that isn't ours to read is skipped, not fatal.
                if let Ok(sealed) = serde_json::from_str::<Sealed>(&op.blob) {
                    out.push(Received {
                        sealed,
                        stored_at: op.at as u64,
                    });
                }
            }
            if !page.more {
                return Ok((out, after));
            }
        }
    }

    /// How big the group is on the relay: batches, bytes and the newest
    /// sequence number (`head`). `None` from a relay without compaction.
    pub async fn stats(&self) -> Result<Option<RelayStats>, SyncError> {
        let resp = self
            .http
            .get(self.url("stats"))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(http_err)?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(check(resp).await?.json().await.map_err(http_err)?))
    }

    /// Replace every batch up to `up_to` with `snapshot` (sealed batches that
    /// rebuild the same state). Only call this after applying everything
    /// through `up_to`; see docs/sync-protocol.md. Returns the batches removed.
    pub async fn compact(&self, up_to: i64, snapshot: &[Sealed]) -> Result<i64, SyncError> {
        let blobs = snapshot
            .iter()
            .map(|s| serde_json::to_string(s).map_err(|e| SyncError::Storage(e.to_string())))
            .collect::<Result<Vec<_>, _>>()?;
        let resp = self
            .writing(self.http.post(self.url("compact")))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "device": self.device, "up_to": up_to, "blobs": blobs }))
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .map_err(http_err)?;
        let reply: serde_json::Value = check(resp).await?.json().await.map_err(http_err)?;
        Ok(reply["removed"].as_i64().unwrap_or(0))
    }

    pub async fn put_device(&self, sealed: &Sealed) -> Result<(), SyncError> {
        let blob = serde_json::to_string(sealed).map_err(|e| SyncError::Storage(e.to_string()))?;
        let resp = self
            .writing(self.http.put(self.url(&format!("devices/{}", self.device))))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "blob": blob }))
            .send()
            .await
            .map_err(http_err)?;
        check(resp).await?;
        Ok(())
    }

    pub async fn devices(&self) -> Result<Vec<Received>, SyncError> {
        let resp = self
            .http
            .get(self.url("devices"))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(http_err)?;
        let page: DevicesPage = check(resp).await?.json().await.map_err(http_err)?;
        Ok(page
            .devices
            .into_iter()
            .filter_map(|d| {
                Some(Received {
                    sealed: serde_json::from_str(&d.blob).ok()?,
                    stored_at: d.at as u64,
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_http_only_stays_on_your_own_network() {
        for ok in [
            "https://relay.example.com",
            "https://203.0.113.5:8443/",
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
            "http://192.168.1.20:8080",
            "http://10.0.0.5",
            "http://172.20.1.1",
            "http://100.101.102.103",
            "http://[fd12:3456::1]",
            "http://nas:8080",
            "http://relay.lan",
            "http://pi.local",
            "http://box.home.arpa",
        ] {
            assert!(check_address(ok).is_ok(), "{ok} should be accepted");
        }
        for bad in [
            "http://relay.example.com",
            "http://203.0.113.5:8080",
            "http://172.32.0.1",
            "http://[2001:db8::1]",
            "ftp://relay.example.com",
            "relay.example.com",
        ] {
            assert!(check_address(bad).is_err(), "{bad} should be refused");
        }
    }
    #[test]
    fn the_websocket_tls_setup_builds() {
        // wss:// uses the system's trust store through an explicit crypto provider;
        // this fails loudly if the provider setup is ever ambiguous.
        assert!(super::tls_config().is_ok());
    }
}
