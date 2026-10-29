//! Saved passwords that sync between computers (opt-in).
//!
//! Each computer keeps its passwords in the OS keychain as before. With
//! password sync on, each one is also a synced record, encrypted with the
//! vault key (from the vault passphrase; see `linecard_vault::synced`).
//! [`reconcile`] keeps the two in step both ways: records from other
//! computers are saved to the keychain, passwords saved here become
//! records, and a record removed elsewhere is removed here too.

use std::collections::BTreeSet;

use linecard_core_model::{SyncedSecret, VaultMeta};
use linecard_vault::synced::{new_salt, VaultKey};
use linecard_vault::Secret;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::sync::SyncEngine;
use crate::{edit, err, AppState, CmdResult};

/// Slots seen as synced last time, to tell "removed elsewhere" from "new here".
const SYNCED_SLOTS: &str = "secrets_synced";

fn key_slot(device: &str) -> String {
    format!("sync:{device}:vault-key")
}

fn device(state: &AppState) -> Option<String> {
    Some(state.lib.lock().ok()?.device().to_string())
}

/// This computer's vault key, if password sync is on here.
fn vault_key(state: &AppState) -> Option<VaultKey> {
    let s = state.vault.get(&key_slot(&device(state)?))?;
    VaultKey::from_hex(s.expose())
}

fn synced_slots(state: &AppState) -> BTreeSet<String> {
    state
        .lib
        .lock()
        .ok()
        .and_then(|l| l.local_setting(SYNCED_SLOTS).ok().flatten())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn set_synced_slots(state: &AppState, slots: &BTreeSet<String>) {
    if let Ok(l) = state.lib.lock() {
        let _ = l.set_local_setting(SYNCED_SLOTS, &serde_json::to_string(slots).unwrap_or_default());
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordSyncDto {
    /// Sync is set up (password sync needs it).
    pub available: bool,
    /// On for this computer.
    pub enabled: bool,
    /// Another computer (or this one) has set a vault passphrase.
    pub exists: bool,
    /// Passwords in the synced vault.
    pub count: usize,
}

#[tauri::command]
pub fn get_password_sync(app: AppHandle, state: State<AppState>) -> CmdResult<PasswordSyncDto> {
    let available = SyncEngine::config(&app).is_some();
    let (meta, count) = {
        let lib = state.lib.lock().map_err(err)?;
        (lib.tree().vault.clone(), lib.tree().secrets.len())
    };
    let enabled = match (&meta, vault_key(&state)) {
        (Some(m), Some(k)) => k.verifies(&m.check),
        _ => false,
    };
    Ok(PasswordSyncDto {
        available,
        enabled,
        exists: meta.is_some(),
        count,
    })
}

/// Turn password sync on here: set the vault passphrase (first computer) or
/// enter it (the others).
#[tauri::command]
pub async fn enable_password_sync(app: AppHandle, passphrase: String) -> CmdResult<()> {
    if SyncEngine::config(&app).is_none() {
        return Err("Turn on sync first (Settings → Sync).".into());
    }
    let state = app.state::<AppState>();
    let meta = state.lib.lock().map_err(err)?.tree().vault.clone();
    let (key, new_meta) = tauri::async_runtime::spawn_blocking(move || -> Result<_, String> {
        match meta {
            Some(m) => {
                let k = VaultKey::derive(&passphrase, &m.salt).map_err(err)?;
                if !k.verifies(&m.check) {
                    return Err("That isn't the vault passphrase set on your other computer.".into());
                }
                Ok((k, None))
            }
            None => {
                let salt = new_salt().map_err(err)?;
                let k = VaultKey::derive(&passphrase, &salt).map_err(err)?;
                let check = k.check_value().map_err(err)?;
                Ok((k, Some(VaultMeta { salt, check })))
            }
        }
    })
    .await
    .map_err(err)??;
    if let Some(m) = new_meta {
        edit(&app, &state, |t| {
            t.vault.get_or_insert(m);
            Ok(())
        })?;
    }
    let device = device(&state).ok_or("the library is unavailable")?;
    state
        .vault
        .save_hidden(&key_slot(&device), Secret::new(key.to_hex()))
        .map_err(err)?;
    reconcile(&app)
}

/// Stop syncing passwords on this computer (they stay in its keychain).
#[tauri::command]
pub fn disable_password_sync(state: State<AppState>) -> CmdResult<()> {
    let device = device(&state).ok_or("the library is unavailable")?;
    state.vault.forget_hidden(&key_slot(&device)).map_err(err)?;
    set_synced_slots(&state, &BTreeSet::new());
    Ok(())
}

/// A password was saved here (from a login prompt).
pub fn after_local_save(app: &AppHandle, slot: &str, label: &str, password: &str) {
    let state = app.state::<AppState>();
    let Some(k) = vault_key(&state) else { return };
    let Ok(blob) = k.encrypt(slot, password) else { return };
    let record = SyncedSecret {
        id: SyncedSecret::id_for(slot),
        key: slot.to_string(),
        label: label.to_string(),
        blob,
    };
    if edit(app, &state, |t| {
        if t.vault.is_some() {
            t.secrets.insert(record.id.clone(), record);
        }
        Ok(())
    })
    .is_ok()
    {
        let mut slots = synced_slots(&state);
        slots.insert(slot.to_string());
        set_synced_slots(&state, &slots);
    }
}

/// A saved password was removed here (Settings → Saved passwords).
pub fn after_local_forget(app: &AppHandle, slot: &str) {
    let state = app.state::<AppState>();
    if vault_key(&state).is_none() {
        return;
    }
    let id = SyncedSecret::id_for(slot);
    let _ = edit(app, &state, |t| {
        t.secrets.remove(&id);
        Ok(())
    });
    let mut slots = synced_slots(&state);
    slots.remove(slot);
    set_synced_slots(&state, &slots);
}

/// Bring the keychain and the synced vault in step (both ways).
pub fn reconcile(app: &AppHandle) -> CmdResult<()> {
    let state = app.state::<AppState>();
    let Some(k) = vault_key(&state) else { return Ok(()) };
    let (meta, records) = {
        let lib = state.lib.lock().map_err(err)?;
        (lib.tree().vault.clone(), lib.tree().secrets.clone())
    };
    let Some(meta) = meta else { return Ok(()) };
    if !k.verifies(&meta.check) {
        return Err("The vault passphrase was changed on another computer; turn password sync on again.".into());
    }
    let before = synced_slots(&state);
    let mut now: BTreeSet<String> = BTreeSet::new();

    // Theirs → here (also when the keychain has it but Settings doesn't list it).
    let listed: BTreeSet<String> = state.vault.list().into_iter().map(|c| c.key).collect();
    for r in records.values() {
        now.insert(r.key.clone());
        let Ok(password) = k.decrypt(&r.key, &r.blob) else {
            continue;
        };
        let stored = state.vault.get(&r.key).map(|s| s.expose().to_string());
        if stored.as_deref() != Some(password.as_str()) || !listed.contains(&r.key) {
            state.vault.save(&r.key, &r.label, Secret::new(password)).map_err(err)?;
        }
    }
    // Removed elsewhere → removed here.
    for gone in before.difference(&now) {
        let _ = state.vault.forget(gone);
    }
    // New here → theirs.
    let uploads: Vec<SyncedSecret> = state
        .vault
        .list()
        .into_iter()
        .filter(|c| !now.contains(&c.key) && !before.contains(&c.key))
        .filter_map(|c| {
            let password = state.vault.get(&c.key)?;
            Some(SyncedSecret {
                id: SyncedSecret::id_for(&c.key),
                blob: k.encrypt(&c.key, password.expose()).ok()?,
                key: c.key,
                label: c.label,
            })
        })
        .collect();
    if !uploads.is_empty() {
        now.extend(uploads.iter().map(|u| u.key.clone()));
        edit(app, &state, |t| {
            for u in uploads {
                t.secrets.insert(u.id.clone(), u);
            }
            Ok(())
        })?;
    }
    set_synced_slots(&state, &now);
    Ok(())
}
