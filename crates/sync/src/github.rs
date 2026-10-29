//! Sync through a GitHub repository (a private one, normally), using the
//! same layout as a shared folder:
//!
//! ```text
//! linecard-<group id>/ops/<device>/000000000001.json   one sealed batch per file
//! linecard-<group id>/devices/<device>.json            latest heartbeat
//! ```
//!
//! Each push is a commit through the contents API. A pull lists the
//! repository tree once and fetches only blobs past the cursor (the last
//! sequence number read from each device). Everything stored is sealed, so
//! the repository only ever holds ciphertext.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine;
use serde::Deserialize;

use crate::folder::FolderCursor;
use crate::{Received, Sealed, SyncError};

pub const API: &str = "https://api.github.com";

pub struct GitHubTransport {
    api: String,
    repo: String,
    token: String,
    root: String,
    device: String,
    http: reqwest::Client,
    /// Branch to use (the repository's default, looked up once).
    branch: Mutex<Option<String>>,
    /// Next sequence number for our own batches (found once, then counted).
    next_seq: Mutex<Option<u64>>,
}

#[derive(Deserialize)]
struct RepoInfo {
    default_branch: String,
    #[serde(default)]
    permissions: Option<Permissions>,
}

#[derive(Deserialize)]
struct Permissions {
    push: bool,
}

#[derive(Deserialize)]
struct Tree {
    tree: Vec<TreeEntry>,
    #[serde(default)]
    truncated: bool,
}

#[derive(Deserialize)]
struct TreeEntry {
    path: String,
    sha: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct Blob {
    content: String,
}

#[derive(Deserialize)]
struct FileInfo {
    sha: String,
}

fn safe(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn http_err(e: reqwest::Error) -> SyncError {
    if e.is_timeout() || e.is_connect() {
        SyncError::Relay(format!("can't reach GitHub ({e})"))
    } else {
        SyncError::Relay(e.to_string())
    }
}

async fn check(resp: reqwest::Response) -> Result<reqwest::Response, SyncError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    let msg = body["message"].as_str().unwrap_or("").to_string();
    Err(SyncError::Relay(match status.as_u16() {
        401 => "GitHub refused the token (expired, or mistyped?)".into(),
        403 if msg.to_lowercase().contains("rate limit") => {
            "GitHub's rate limit is used up; sync resumes shortly".into()
        }
        403 => format!("the token can't write to this repository ({msg})"),
        404 => "repository not found, or the token can't see it (it needs Contents: read and write)".into(),
        _ => format!("GitHub: {status} {msg}"),
    }))
}

impl GitHubTransport {
    /// `repo` like "owner/name". `api` is [`API`] except in tests.
    pub fn new(api: &str, repo: &str, token: &str, group_id: &str, device: &str) -> Result<Self, SyncError> {
        let repo = repo
            .trim()
            .trim_start_matches("https://github.com/")
            .trim_end_matches('/')
            .trim_end_matches(".git")
            .to_string();
        let parts: Vec<&str> = repo.split('/').collect();
        if parts.len() != 2 || parts.iter().any(|p| p.is_empty()) {
            return Err(SyncError::Relay("enter the repository as owner/name".into()));
        }
        if token.trim().is_empty() {
            return Err(SyncError::Relay("enter a GitHub token".into()));
        }
        Ok(GitHubTransport {
            api: api.trim_end_matches('/').to_string(),
            repo,
            token: token.trim().to_string(),
            root: format!("linecard-{group_id}"),
            device: safe(device),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .user_agent("Linecard")
                .build()
                .map_err(http_err)?,
            branch: Mutex::new(None),
            next_seq: Mutex::new(None),
        })
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}/repos/{}/{path}", self.api, self.repo))
            .bearer_auth(&self.token)
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
    }

    async fn branch(&self) -> Result<String, SyncError> {
        if let Some(b) = self.branch.lock().ok().and_then(|b| b.clone()) {
            return Ok(b);
        }
        self.check().await?;
        Ok(self
            .branch
            .lock()
            .ok()
            .and_then(|b| b.clone())
            .unwrap_or_else(|| "main".into()))
    }

    /// The repository exists, the token can write to it, and it has a branch.
    pub async fn check(&self) -> Result<(), SyncError> {
        let resp = self
            .http
            .get(format!("{}/repos/{}", self.api, self.repo))
            .bearer_auth(&self.token)
            .header("accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(http_err)?;
        let info: RepoInfo = check(resp).await?.json().await.map_err(http_err)?;
        if info.permissions.is_some_and(|p| !p.push) {
            return Err(SyncError::Relay(
                "the token can read but not write this repository".into(),
            ));
        }
        if let Ok(mut b) = self.branch.lock() {
            *b = Some(info.default_branch);
        }
        Ok(())
    }

    /// Every file under our group folder: path → blob sha. An empty
    /// repository (no commits yet) has no tree.
    async fn files(&self) -> Result<BTreeMap<String, String>, SyncError> {
        let branch = self.branch().await?;
        let resp = self
            .req(reqwest::Method::GET, &format!("git/trees/{branch}?recursive=1"))
            .send()
            .await
            .map_err(http_err)?;
        if resp.status().as_u16() == 404 || resp.status().as_u16() == 409 {
            return Ok(BTreeMap::new());
        }
        let tree: Tree = check(resp).await?.json().await.map_err(http_err)?;
        if tree.truncated {
            return Err(SyncError::Relay(
                "the repository is too large to list; use one just for Linecard".into(),
            ));
        }
        let prefix = format!("{}/", self.root);
        Ok(tree
            .tree
            .into_iter()
            .filter(|e| e.kind == "blob" && e.path.starts_with(&prefix))
            .map(|e| (e.path[prefix.len()..].to_string(), e.sha))
            .collect())
    }

    async fn blob(&self, sha: &str) -> Result<Sealed, SyncError> {
        let resp = self
            .req(reqwest::Method::GET, &format!("git/blobs/{sha}"))
            .send()
            .await
            .map_err(http_err)?;
        let blob: Blob = check(resp).await?.json().await.map_err(http_err)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(blob.content.replace(['\n', '\r'], ""))
            .map_err(|e| SyncError::Corrupt(e.to_string()))?;
        serde_json::from_slice(&bytes).map_err(|e| SyncError::Corrupt(e.to_string()))
    }

    /// Create or replace one file (one commit).
    async fn put_file(
        &self,
        rel: &str,
        sealed: &Sealed,
        replace: Option<String>,
    ) -> Result<reqwest::StatusCode, SyncError> {
        let body = serde_json::to_vec(sealed).map_err(|e| SyncError::Storage(e.to_string()))?;
        let mut req = serde_json::json!({
            "message": format!("linecard: {rel}"),
            "content": base64::engine::general_purpose::STANDARD.encode(body),
            "branch": self.branch().await?,
        });
        if let Some(sha) = replace {
            req["sha"] = sha.into();
        }
        let resp = self
            .req(reqwest::Method::PUT, &format!("contents/{}/{rel}", self.root))
            .json(&req)
            .send()
            .await
            .map_err(http_err)?;
        let status = resp.status();
        if status.as_u16() == 422 || status.as_u16() == 409 {
            return Ok(status);
        }
        check(resp).await?;
        Ok(status)
    }

    pub async fn push(&self, sealed: &Sealed) -> Result<(), SyncError> {
        for _attempt in 0..3 {
            let seq = match self.next_seq.lock().ok().and_then(|n| *n) {
                Some(n) => n,
                None => {
                    let mine = format!("ops/{}/", self.device);
                    self.files()
                        .await?
                        .keys()
                        .filter_map(|p| p.strip_prefix(&mine)?.strip_suffix(".json")?.parse::<u64>().ok())
                        .max()
                        .unwrap_or(0)
                        + 1
                }
            };
            let rel = format!("ops/{}/{seq:012}.json", self.device);
            let status = self.put_file(&rel, sealed, None).await?;
            if status.is_success() {
                if let Ok(mut n) = self.next_seq.lock() {
                    *n = Some(seq + 1);
                }
                return Ok(());
            }
            // The number was taken (another install of this device?): recount.
            if let Ok(mut n) = self.next_seq.lock() {
                *n = None;
            }
        }
        Err(SyncError::Relay(
            "couldn't write to the repository (conflicting writes)".into(),
        ))
    }

    /// Batches past the cursor from every other device. Like a shared folder,
    /// each device's batches are read in order and a gap stops that device.
    pub async fn pull(&self, cursor: &FolderCursor) -> Result<(Vec<Received>, FolderCursor), SyncError> {
        let files = self.files().await?;
        let mut by_device: BTreeMap<String, BTreeMap<u64, String>> = BTreeMap::new();
        for (path, sha) in &files {
            let Some(rest) = path.strip_prefix("ops/") else {
                continue;
            };
            let Some((device, name)) = rest.split_once('/') else {
                continue;
            };
            if device == self.device {
                continue;
            }
            if let Some(seq) = name.strip_suffix(".json").and_then(|s| s.parse::<u64>().ok()) {
                by_device
                    .entry(device.to_string())
                    .or_default()
                    .insert(seq, sha.clone());
            }
        }
        let mut out = Vec::new();
        let mut next = cursor.clone();
        for (device, seqs) in by_device {
            let mut last = cursor.get(&device).copied().unwrap_or(0);
            while let Some(sha) = seqs.get(&(last + 1)) {
                out.push(Received {
                    sealed: self.blob(sha).await?,
                    stored_at: 0,
                });
                last += 1;
            }
            next.insert(device, last);
        }
        Ok((out, next))
    }

    pub async fn put_device(&self, sealed: &Sealed) -> Result<(), SyncError> {
        let rel = format!("devices/{}.json", self.device);
        let existing = self
            .req(
                reqwest::Method::GET,
                &format!("contents/{}/{rel}?ref={}", self.root, self.branch().await?),
            )
            .send()
            .await
            .map_err(http_err)?;
        let sha = if existing.status().as_u16() == 404 {
            None
        } else {
            Some(check(existing).await?.json::<FileInfo>().await.map_err(http_err)?.sha)
        };
        let status = self.put_file(&rel, sealed, sha).await?;
        if !status.is_success() {
            return Err(SyncError::Relay(
                "the heartbeat changed underneath us; will retry".into(),
            ));
        }
        Ok(())
    }

    pub async fn devices(&self) -> Result<Vec<Received>, SyncError> {
        let files = self.files().await?;
        let mut out = Vec::new();
        for (path, sha) in files.iter().filter(|(p, _)| p.starts_with("devices/")) {
            if path.ends_with(".json") {
                if let Ok(sealed) = self.blob(sha).await {
                    out.push(Received { sealed, stored_at: 0 });
                }
            }
        }
        Ok(out)
    }
}
