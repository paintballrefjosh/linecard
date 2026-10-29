//! The synced vault: saved passwords encrypted with a key derived from a
//! vault passphrase, so they can travel between computers as ordinary
//! (already end-to-end encrypted) sync data. Someone with only the sync
//! phrase still can't read them.
//!
//! Key: Argon2id (64 MiB, 3 passes) over the passphrase and a random salt
//! that syncs with the vault. Each password: XChaCha20-Poly1305 with a
//! random nonce, bound to its credential key (AAD), stored as
//! base64(nonce || ciphertext). A known value encrypted the same way lets
//! another computer tell a wrong passphrase from a right one.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

use crate::VaultError;

const CHECK_AAD: &str = "linecard-vault-check";
const CHECK_TEXT: &str = "linecard";

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn random<const N: usize>() -> Result<[u8; N], VaultError> {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).map_err(|e| VaultError::Backend(e.to_string()))?;
    Ok(b)
}

pub struct VaultKey([u8; 32]);

impl std::fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VaultKey(***)")
    }
}

/// A new random salt (base64).
pub fn new_salt() -> Result<String, VaultError> {
    Ok(b64().encode(random::<16>()?))
}

impl VaultKey {
    /// Slow on purpose (about half a second); run it off the UI thread.
    pub fn derive(passphrase: &str, salt: &str) -> Result<Self, VaultError> {
        if passphrase.chars().count() < 8 {
            return Err(VaultError::Backend("use at least 8 characters".into()));
        }
        let salt = b64().decode(salt).map_err(|e| VaultError::Backend(e.to_string()))?;
        let params = Params::new(64 * 1024, 3, 1, Some(32)).map_err(|e| VaultError::Backend(e.to_string()))?;
        let mut key = [0u8; 32];
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(passphrase.as_bytes(), &salt, &mut key)
            .map_err(|e| VaultError::Backend(e.to_string()))?;
        Ok(VaultKey(key))
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        let v: Vec<u8> = (0..s.len())
            .step_by(2)
            .filter_map(|i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
            .collect();
        Some(VaultKey(v.try_into().ok()?))
    }

    pub fn encrypt(&self, aad: &str, plain: &str) -> Result<String, VaultError> {
        let nonce = random::<24>()?;
        let ct = XChaCha20Poly1305::new((&self.0).into())
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plain.as_bytes(),
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| VaultError::Backend("encryption failed".into()))?;
        let mut out = nonce.to_vec();
        out.extend(ct);
        Ok(b64().encode(out))
    }

    pub fn decrypt(&self, aad: &str, blob: &str) -> Result<String, VaultError> {
        let raw = b64().decode(blob).map_err(|_| VaultError::Locked)?;
        if raw.len() < 24 {
            return Err(VaultError::Locked);
        }
        let plain = XChaCha20Poly1305::new((&self.0).into())
            .decrypt(
                XNonce::from_slice(&raw[..24]),
                Payload {
                    msg: &raw[24..],
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| VaultError::Locked)?;
        String::from_utf8(plain).map_err(|_| VaultError::Locked)
    }

    /// Stored with the vault so other computers can check a passphrase.
    pub fn check_value(&self) -> Result<String, VaultError> {
        self.encrypt(CHECK_AAD, CHECK_TEXT)
    }

    pub fn verifies(&self, check: &str) -> bool {
        self.decrypt(CHECK_AAD, check).is_ok_and(|t| t == CHECK_TEXT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_wrong_passphrase() {
        let salt = new_salt().unwrap();
        let key = VaultKey::derive("correct horse battery", &salt).unwrap();
        let check = key.check_value().unwrap();
        let blob = key.encrypt("ssh:admin@sw1:22", "hunter2").unwrap();
        assert!(!blob.contains("hunter2"));

        // Same passphrase and salt on another computer: same key.
        let again = VaultKey::derive("correct horse battery", &salt).unwrap();
        assert!(again.verifies(&check));
        assert_eq!(again.decrypt("ssh:admin@sw1:22", &blob).unwrap(), "hunter2");
        // Bound to its credential: can't be moved to another one.
        assert!(again.decrypt("ssh:admin@sw2:22", &blob).is_err());

        let wrong = VaultKey::derive("incorrect horse battery", &salt).unwrap();
        assert!(!wrong.verifies(&check));
        assert!(wrong.decrypt("ssh:admin@sw1:22", &blob).is_err());

        assert!(VaultKey::derive("short", &salt).is_err());
        let hex = key.to_hex();
        assert!(VaultKey::from_hex(&hex).unwrap().verifies(&check));
    }
}
