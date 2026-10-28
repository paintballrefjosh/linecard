//! Sync through a Linecard relay (see `relay/`). The relay stores sealed
//! blobs per group in arrival order; the cursor is the last sequence number
//! seen.

use std::time::Duration;

use serde::Deserialize;

use crate::{Received, Sealed, SyncError};

pub struct RelayTransport {
    base: String,
    group: String,
    token: String,
    device: String,
    http: reqwest::Client,
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

async fn check(resp: reqwest::Response) -> Result<reqwest::Response, SyncError> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    let msg = body["error"].as_str().unwrap_or("").to_string();
    Err(SyncError::Relay(match status.as_u16() {
        401 => "the relay refused this sync phrase (the group belongs to another phrase)".into(),
        507 => "the relay's storage limit for this group is reached".into(),
        _ => format!("{status} {msg}"),
    }))
}

impl RelayTransport {
    /// `base` like `https://relay.example.com`.
    pub fn new(base: &str, group_id: &str, token: &str, device: &str) -> Result<Self, SyncError> {
        let base = base.trim().trim_end_matches('/').to_string();
        if !(base.starts_with("https://") || base.starts_with("http://")) {
            return Err(SyncError::Relay("the relay address must start with https://".into()));
        }
        Ok(RelayTransport {
            base,
            group: group_id.to_string(),
            token: token.to_string(),
            device: device.to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .map_err(http_err)?,
        })
    }

    fn url(&self, rest: &str) -> String {
        format!("{}/v1/groups/{}/{rest}", self.base, self.group)
    }

    pub async fn push(&self, sealed: &Sealed) -> Result<(), SyncError> {
        let blob = serde_json::to_string(sealed).map_err(|e| SyncError::Storage(e.to_string()))?;
        let resp = self
            .http
            .post(self.url("ops"))
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

    pub async fn put_device(&self, sealed: &Sealed) -> Result<(), SyncError> {
        let blob = serde_json::to_string(sealed).map_err(|e| SyncError::Storage(e.to_string()))?;
        let resp = self
            .http
            .put(self.url(&format!("devices/{}", self.device)))
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
