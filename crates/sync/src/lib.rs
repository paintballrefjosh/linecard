//! Moving the op log between devices, end-to-end encrypted.
//!
//! The merge itself lives in `linecard-core-sync`; this crate only gets
//! ops from one replica to another without the storage learning anything:
//!
//! * [`crypto`]: the group key (a 24-word phrase), device signing keys, and
//!   sealed payloads.
//! * [`folder`]: sync through a folder every device can reach (a NAS share,
//!   OneDrive, Dropbox). Each device writes only its own files.
//! * [`relay`]: sync through a Linecard relay over HTTPS.
//!
//! Both transports store only [`crypto::Sealed`] blobs plus a group id
//! derived from the key.

pub mod crypto;
pub mod folder;
pub mod relay;

use linecard_core_sync::Op;
use serde::{Deserialize, Serialize};

pub use crypto::{DeviceKey, GroupKey, Sealed};

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("that isn't a valid sync phrase: {0}")]
    BadPhrase(String),
    #[error("data from another device was altered or forged")]
    Tampered,
    #[error("data from another device can't be decrypted with this phrase")]
    WrongKey,
    #[error("data from another device is unreadable: {0}")]
    Corrupt(String),
    #[error("data from a newer Linecard (format {0}); update this device")]
    Unsupported(u32),
    #[error("sync storage: {0}")]
    Storage(String),
    #[error("relay: {0}")]
    Relay(String),
}

impl From<std::io::Error> for SyncError {
    fn from(e: std::io::Error) -> Self {
        SyncError::Storage(e.to_string())
    }
}

/// One push: the ops a device made since its last push.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Batch {
    pub ops: Vec<Op>,
}

/// Posted in a group when someone replaces its phrase (to remove a
/// computer, or because the phrase leaked): the group is closed and the
/// others need the new phrase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    /// Name of the computer that closed the group.
    pub closed_by: String,
    pub at: u64,
}

/// A device's heartbeat, shown on the Devices page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub os: String,
    pub app_version: String,
    pub protocol_version: u32,
    /// Unix ms when the device sent this.
    pub sent_at: u64,
}

/// A sealed item read back from storage, with where it came from.
#[derive(Debug, Clone)]
pub struct Received {
    pub sealed: Sealed,
    /// Unix ms the storage saw it (the relay's clock, or the file time).
    pub stored_at: u64,
}
