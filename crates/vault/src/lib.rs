//! Credential vault.
//!
//! Secrets never live in connection objects; connections hold a
//! `credential_id`. Where the secret itself lives depends on the user's
//! per-sync-group preference:
//!
//! * **Local only** (default): stored in the OS keychain under
//!   `linecard/<credential_id>`. Other devices see the credential as
//!   "not on this device".
//! * **Synced**: encrypted with a key derived from a separate vault
//!   passphrase (Argon2id → XChaCha20-Poly1305) and synced as an opaque blob.
//!   Each device caches the unlocked key in its OS keychain.
//!
//! A credential can be pinned *local only* even when sync is on.

use std::collections::HashMap;
use std::fmt;

/// A secret value. Debug output is redacted so it never lands in logs.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Secret(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPolicy {
    LocalOnly,
    Synced,
}

#[derive(Debug)]
pub enum VaultError {
    NotFound(String),
    Locked,
    Backend(String),
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VaultError::NotFound(id) => write!(f, "credential {id} is not on this device"),
            VaultError::Locked => f.write_str("vault is locked"),
            VaultError::Backend(e) => write!(f, "keychain error: {e}"),
        }
    }
}

impl std::error::Error for VaultError {}

/// Storage for secrets. The OS keychain backend lands in Phase 1; the
/// in-memory one is for tests and the browser dev UI.
pub trait SecretStore: Send + Sync {
    fn get(&self, credential_id: &str) -> Result<Secret, VaultError>;
    fn set(&mut self, credential_id: &str, secret: Secret) -> Result<(), VaultError>;
    fn delete(&mut self, credential_id: &str) -> Result<(), VaultError>;
}

#[derive(Default)]
pub struct MemoryStore {
    items: HashMap<String, Secret>,
}

impl SecretStore for MemoryStore {
    fn get(&self, id: &str) -> Result<Secret, VaultError> {
        self.items
            .get(id)
            .cloned()
            .ok_or_else(|| VaultError::NotFound(id.into()))
    }
    fn set(&mut self, id: &str, secret: Secret) -> Result<(), VaultError> {
        self.items.insert(id.into(), secret);
        Ok(())
    }
    fn delete(&mut self, id: &str) -> Result<(), VaultError> {
        self.items.remove(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_redacted_and_stored() {
        let mut s = MemoryStore::default();
        s.set("c1", Secret::new("hunter2")).unwrap();
        assert_eq!(format!("{:?}", s.get("c1").unwrap()), "Secret(***)");
        assert_eq!(s.get("c1").unwrap().expose(), "hunter2");
        s.delete("c1").unwrap();
        assert!(matches!(s.get("c1"), Err(VaultError::NotFound(_))));
    }
}
