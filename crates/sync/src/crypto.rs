//! Keys and sealed (encrypted + signed) payloads.
//!
//! * [`GroupKey`]: 256 random bits shared by every device in a sync group,
//!   shown to people as 24 words (BIP-39). Everything else is derived from
//!   it with HKDF-SHA256: the group id the storage sees, the relay token,
//!   and the encryption key.
//! * [`DeviceKey`]: an Ed25519 key per device; batches are signed so each
//!   change can be attributed and tampering by the storage is detected.
//! * [`Sealed`]: XChaCha20-Poly1305 ciphertext (random 24-byte nonce) with
//!   the group id and sender bound in as associated data, plus a signature
//!   over everything the reader checks.

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::SyncError;

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("the OS random number generator failed");
    b
}

/// The secret every device in a sync group shares.
#[derive(Clone, PartialEq, Eq)]
pub struct GroupKey([u8; 32]);

impl std::fmt::Debug for GroupKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GroupKey({})", self.group_id())
    }
}

impl GroupKey {
    pub fn generate() -> Self {
        GroupKey(random())
    }

    pub fn from_bytes(b: [u8; 32]) -> Self {
        GroupKey(b)
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }

    /// The 24 words people write down or paste on another computer.
    pub fn phrase(&self) -> String {
        bip39::Mnemonic::from_entropy(&self.0)
            .expect("32 bytes is valid BIP-39 entropy")
            .to_string()
    }

    /// Accepts any case and spacing (pasted from an email, a password
    /// manager…); the BIP-39 checksum catches a mistyped word.
    pub fn from_phrase(phrase: &str) -> Result<Self, SyncError> {
        let normalized = phrase.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        let m = bip39::Mnemonic::parse_normalized(&normalized).map_err(|e| SyncError::BadPhrase(e.to_string()))?;
        let entropy = m.to_entropy();
        let bytes: [u8; 32] = entropy
            .try_into()
            .map_err(|_| SyncError::BadPhrase("a sync phrase has 24 words".into()))?;
        Ok(GroupKey(bytes))
    }

    fn derive(&self, info: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        Hkdf::<Sha256>::new(None, &self.0)
            .expand(info.as_bytes(), &mut out)
            .expect("32 bytes is a valid HKDF length");
        out
    }

    /// What the folder or relay knows the group by (reveals nothing about
    /// the key).
    pub fn group_id(&self) -> String {
        self.derive("linecard group id v1")[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Bearer token for the relay: proves membership without the key.
    pub fn relay_token(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.derive("linecard relay token v1"))
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(&self.derive("linecard ops key v1").into())
    }
}

/// A device's signing key.
pub struct DeviceKey(SigningKey);

impl DeviceKey {
    pub fn generate() -> Self {
        DeviceKey(SigningKey::from_bytes(&random()))
    }

    pub fn from_bytes(b: [u8; 32]) -> Self {
        DeviceKey(SigningKey::from_bytes(&b))
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    pub fn public(&self) -> String {
        STANDARD.encode(self.0.verifying_key().to_bytes())
    }
}

/// An encrypted, signed payload as stored in the folder or on the relay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sealed {
    /// Format version.
    pub v: u32,
    pub device: String,
    /// Sender's Ed25519 public key (base64).
    pub key: String,
    pub nonce: String,
    pub data: String,
    pub sig: String,
}

fn aad(group_id: &str, device: &str) -> Vec<u8> {
    format!("{group_id}\0{device}").into_bytes()
}

fn signed_bytes(group_id: &str, device: &str, nonce: &[u8], data: &[u8]) -> Vec<u8> {
    let mut m = aad(group_id, device);
    m.push(0);
    m.extend_from_slice(nonce);
    m.extend_from_slice(data);
    m
}

pub fn seal<T: Serialize>(group: &GroupKey, device_key: &DeviceKey, device: &str, payload: &T) -> Sealed {
    let gid = group.group_id();
    let nonce: [u8; 24] = random();
    let plain = serde_json::to_vec(payload).expect("payload serializes");
    let data = group
        .cipher()
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &plain,
                aad: &aad(&gid, device),
            },
        )
        .expect("encryption can't fail with a valid key");
    let sig = device_key.0.sign(&signed_bytes(&gid, device, &nonce, &data));
    Sealed {
        v: 1,
        device: device.to_string(),
        key: device_key.public(),
        nonce: STANDARD.encode(nonce),
        data: STANDARD.encode(&data),
        sig: STANDARD.encode(sig.to_bytes()),
    }
}

/// Check the signature, decrypt, and parse. Returns the payload; the
/// sender's public key is `sealed.key` (pin it per device to detect a
/// different key later).
pub fn open<T: DeserializeOwned>(group: &GroupKey, sealed: &Sealed) -> Result<T, SyncError> {
    if sealed.v != 1 {
        return Err(SyncError::Unsupported(sealed.v));
    }
    let b64 = |s: &str| STANDARD.decode(s).map_err(|_| SyncError::Tampered);
    let gid = group.group_id();
    let nonce = b64(&sealed.nonce)?;
    let data = b64(&sealed.data)?;
    let key: [u8; 32] = b64(&sealed.key)?.try_into().map_err(|_| SyncError::Tampered)?;
    let sig: [u8; 64] = b64(&sealed.sig)?.try_into().map_err(|_| SyncError::Tampered)?;
    if nonce.len() != 24 {
        return Err(SyncError::Tampered);
    }
    VerifyingKey::from_bytes(&key)
        .map_err(|_| SyncError::Tampered)?
        .verify(
            &signed_bytes(&gid, &sealed.device, &nonce, &data),
            &Signature::from_bytes(&sig),
        )
        .map_err(|_| SyncError::Tampered)?;
    let plain = group
        .cipher()
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &data,
                aad: &aad(&gid, &sealed.device),
            },
        )
        .map_err(|_| SyncError::WrongKey)?;
    serde_json::from_slice(&plain).map_err(|e| SyncError::Corrupt(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrases_round_trip_and_tolerate_formatting() {
        let k = GroupKey::generate();
        let phrase = k.phrase();
        assert_eq!(phrase.split(' ').count(), 24);
        assert_eq!(GroupKey::from_phrase(&phrase).unwrap(), k);
        let messy = format!("  {}\n", phrase.to_uppercase().replace(' ', "  \t"));
        assert_eq!(GroupKey::from_phrase(&messy).unwrap(), k);
        // One wrong word fails the checksum.
        let mut words: Vec<&str> = phrase.split(' ').collect();
        words[3] = if words[3] == "abandon" { "ability" } else { "abandon" };
        assert!(GroupKey::from_phrase(&words.join(" ")).is_err());
        assert!(GroupKey::from_phrase("too short").is_err());
    }

    #[test]
    fn derived_values_are_stable_and_distinct() {
        let k = GroupKey::from_bytes([7; 32]);
        assert_eq!(k.group_id(), GroupKey::from_bytes([7; 32]).group_id());
        assert_eq!(k.group_id().len(), 32);
        assert_ne!(k.group_id(), GroupKey::from_bytes([8; 32]).group_id());
        assert!(!k.relay_token().contains(&k.group_id()));
    }

    #[test]
    fn seal_open_and_reject_tampering() {
        let group = GroupKey::generate();
        let dev = DeviceKey::generate();
        let sealed = seal(&group, &dev, "laptop", &vec!["op1", "op2"]);
        let back: Vec<String> = open(&group, &sealed).unwrap();
        assert_eq!(back, vec!["op1", "op2"]);
        assert!(!sealed.data.contains("op1"), "encrypted");

        // A different group can't read it.
        assert!(matches!(
            open::<Vec<String>>(&GroupKey::generate(), &sealed),
            Err(SyncError::WrongKey | SyncError::Tampered)
        ));
        // Claiming another device breaks the signature.
        let mut forged = sealed.clone();
        forged.device = "desktop".into();
        assert!(matches!(open::<Vec<String>>(&group, &forged), Err(SyncError::Tampered)));
        // Swapping in another key without re-signing fails too.
        let mut swapped = sealed.clone();
        swapped.key = DeviceKey::generate().public();
        assert!(matches!(
            open::<Vec<String>>(&group, &swapped),
            Err(SyncError::Tampered)
        ));
        // Flipped ciphertext.
        let mut bytes = STANDARD.decode(&sealed.data).unwrap();
        bytes[0] ^= 1;
        let mut flipped = sealed.clone();
        flipped.data = STANDARD.encode(bytes);
        assert!(open::<Vec<String>>(&group, &flipped).is_err());
    }

    #[test]
    fn device_keys_persist() {
        let d = DeviceKey::generate();
        assert_eq!(DeviceKey::from_bytes(d.to_bytes()).public(), d.public());
    }
}
