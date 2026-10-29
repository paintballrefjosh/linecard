//! Saved passwords that sync (opt-in): each one encrypted with the vault
//! key, which only computers that know the vault passphrase have.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::ObjectId;

/// The synced vault's public parts: the key's salt and a check value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct VaultMeta {
    pub salt: String,
    pub check: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SyncedSecret {
    pub id: ObjectId,
    /// The saved-password slot, e.g. "ssh:admin@10.0.0.1:22".
    pub key: String,
    /// What Settings shows, e.g. "admin@10.0.0.1".
    pub label: String,
    /// The password, encrypted with the vault key.
    pub blob: String,
}

impl SyncedSecret {
    /// Object id for a saved-password slot (stable, so two computers saving
    /// the same slot write the same object).
    pub fn id_for(key: &str) -> ObjectId {
        // FNV-1a: no dependencies, and the slot text itself is in the object.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in key.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        format!("secret-{h:016x}")
    }
}
