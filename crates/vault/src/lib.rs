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

pub mod synced;

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

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

/// The OS keychain: Windows Credential Manager, macOS Keychain, Linux
/// keyutils. Entries are named `linecard` / `<credential id>`.
pub struct KeychainStore;

const SERVICE: &str = "linecard";

fn keychain_err(e: keyring::Error) -> VaultError {
    VaultError::Backend(e.to_string())
}

impl SecretStore for KeychainStore {
    fn get(&self, id: &str) -> Result<Secret, VaultError> {
        let entry = keyring::Entry::new(SERVICE, id).map_err(keychain_err)?;
        match entry.get_password() {
            Ok(p) => Ok(Secret(p)),
            Err(keyring::Error::NoEntry) => Err(VaultError::NotFound(id.into())),
            Err(e) => Err(keychain_err(e)),
        }
    }
    fn set(&mut self, id: &str, secret: Secret) -> Result<(), VaultError> {
        keyring::Entry::new(SERVICE, id)
            .and_then(|e| e.set_password(secret.expose()))
            .map_err(keychain_err)
    }
    fn delete(&mut self, id: &str) -> Result<(), VaultError> {
        match keyring::Entry::new(SERVICE, id).and_then(|e| e.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(keychain_err(e)),
        }
    }
}

/// What the user sees in Settings → Saved passwords. Never the secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedCredential {
    pub key: String,
    /// e.g. "admin@10.0.0.1"
    pub label: String,
    /// Unix ms
    pub saved_at: u64,
}

/// Saved secrets plus an index of what is saved (keychains can't be
/// listed portably). Safe to share between sessions.
pub struct Vault {
    store: Mutex<Box<dyn SecretStore>>,
    index: Mutex<Vec<SavedCredential>>,
    index_path: Option<PathBuf>,
}

impl Vault {
    /// The OS keychain, with the index kept at `index_path` (a JSON file).
    pub fn keychain(index_path: PathBuf) -> Self {
        Self::new(Box::new(KeychainStore), Some(index_path))
    }

    pub fn in_memory() -> Self {
        Self::new(Box::<MemoryStore>::default(), None)
    }

    pub fn new(store: Box<dyn SecretStore>, index_path: Option<PathBuf>) -> Self {
        let index = index_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Vault {
            store: Mutex::new(store),
            index: Mutex::new(index),
            index_path,
        }
    }

    pub fn get(&self, key: &str) -> Option<Secret> {
        self.store.lock().ok()?.get(key).ok()
    }

    pub fn save(&self, key: &str, label: &str, secret: Secret) -> Result<(), VaultError> {
        self.store.lock().map_err(|_| VaultError::Locked)?.set(key, secret)?;
        let saved_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.update_index(|idx| {
            idx.retain(|c| c.key != key);
            idx.push(SavedCredential {
                key: key.into(),
                label: label.into(),
                saved_at,
            });
        })
    }

    /// Store a secret that isn't a saved password (sync keys): kept in the
    /// keychain but not listed in Settings.
    pub fn save_hidden(&self, key: &str, secret: Secret) -> Result<(), VaultError> {
        self.store.lock().map_err(|_| VaultError::Locked)?.set(key, secret)
    }

    pub fn forget_hidden(&self, key: &str) -> Result<(), VaultError> {
        self.store.lock().map_err(|_| VaultError::Locked)?.delete(key)
    }

    pub fn forget(&self, key: &str) -> Result<(), VaultError> {
        self.store.lock().map_err(|_| VaultError::Locked)?.delete(key)?;
        self.update_index(|idx| idx.retain(|c| c.key != key))
    }

    /// Saved entries, most recent first.
    pub fn list(&self) -> Vec<SavedCredential> {
        let mut v = self.index.lock().map(|i| i.clone()).unwrap_or_default();
        v.sort_by_key(|c| std::cmp::Reverse(c.saved_at));
        v
    }

    fn update_index(&self, f: impl FnOnce(&mut Vec<SavedCredential>)) -> Result<(), VaultError> {
        let mut idx = self.index.lock().map_err(|_| VaultError::Locked)?;
        f(&mut idx);
        if let Some(path) = &self.index_path {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let json = serde_json::to_string_pretty(&*idx).map_err(|e| VaultError::Backend(e.to_string()))?;
            std::fs::write(path, json).map_err(|e| VaultError::Backend(e.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_indexes_without_storing_secrets() {
        let dir = std::env::temp_dir().join(format!("linecard-vault-test-{}", std::process::id()));
        let index = dir.join("credentials.json");
        let _ = std::fs::remove_dir_all(&dir);
        {
            let v = Vault::new(Box::<MemoryStore>::default(), Some(index.clone()));
            v.save("ssh:admin@sw1:22", "admin@sw1", Secret::new("hunter2")).unwrap();
            v.save("ssh:admin@sw2:22", "admin@sw2", Secret::new("swordfish"))
                .unwrap();
            v.save("ssh:admin@sw1:22", "admin@sw1", Secret::new("hunter3")).unwrap();
            assert_eq!(v.get("ssh:admin@sw1:22").unwrap().expose(), "hunter3");
            assert_eq!(v.list().len(), 2, "re-saving replaces the entry");
            v.forget("ssh:admin@sw2:22").unwrap();
            assert!(v.get("ssh:admin@sw2:22").is_none());
        }
        let on_disk = std::fs::read_to_string(&index).unwrap();
        assert!(on_disk.contains("admin@sw1"));
        assert!(!on_disk.contains("hunter"), "the index never holds secrets");
        let reopened = Vault::new(Box::<MemoryStore>::default(), Some(index));
        assert_eq!(reopened.list().len(), 1, "index survives a restart");
    }

    /// Touches the real OS keychain, so it only runs on request:
    /// `cargo test -p linecard-vault -- --ignored`
    #[test]
    #[ignore]
    fn os_keychain_round_trip() {
        let mut k = KeychainStore;
        let id = "linecard-selftest";
        k.set(id, Secret::new("selftest-value")).unwrap();
        assert_eq!(k.get(id).unwrap().expose(), "selftest-value");
        k.delete(id).unwrap();
        assert!(matches!(k.get(id), Err(VaultError::NotFound(_))));
        k.delete(id).unwrap(); // deleting twice is fine
    }

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
