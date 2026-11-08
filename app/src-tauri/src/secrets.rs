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
        // Most often two computers turned password sync on at the same moment
        // and each made a vault; one passphrase wins, and this is the other.
        // Dropping the key makes Settings ask for the winning passphrase.
        if let Some(device) = device(&state) {
            let _ = state.vault.forget_hidden(&key_slot(&device));
        }
        return Err("Another computer set the vault passphrase at the same time, and its passphrase is the one in use. Enter that passphrase in Settings to turn password sync on here.".into());
    }
    let before = synced_slots(&state);
    let mut now: BTreeSet<String> = BTreeSet::new();

    // Theirs → here (also when the keychain has it but Settings doesn't list it).
    let listed: BTreeSet<String> = state.vault.list().into_iter().map(|c| c.key).collect();
    let mut unreadable: Vec<SyncedSecret> = Vec::new();
    for r in records.values() {
        now.insert(r.key.clone());
        let Ok(password) = k.decrypt(&r.key, &r.blob) else {
            unreadable.push(r.clone());
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
    // Records made under a vault key that lost a race can't be read by anyone
    // now: re-save them from this keychain when it has the password, else drop them.
    let (resaved, dropped) = repair_unreadable(&k, &unreadable, |slot| {
        state.vault.get(slot).map(|s| s.expose().to_string())
    });
    if !resaved.is_empty() || !dropped.is_empty() {
        edit(app, &state, |t| {
            for r in resaved {
                t.secrets.insert(r.id.clone(), r);
            }
            for id in dropped {
                t.secrets.remove(&id);
            }
            Ok(())
        })?;
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

/// What to do with records that the current vault key can't decrypt: the
/// ones this computer still has a password for are encrypted again under the
/// current key (replacing them); the others are dropped, since nothing can
/// read them. Returns (records to store, ids to remove).
fn repair_unreadable(
    key: &VaultKey,
    unreadable: &[SyncedSecret],
    local: impl Fn(&str) -> Option<String>,
) -> (Vec<SyncedSecret>, Vec<String>) {
    let mut resaved = Vec::new();
    let mut dropped = Vec::new();
    for r in unreadable {
        match local(&r.key).and_then(|pw| key.encrypt(&r.key, &pw).ok()) {
            Some(blob) => resaved.push(SyncedSecret { blob, ..r.clone() }),
            None => dropped.push(r.id.clone()),
        }
    }
    (resaved, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(key: &VaultKey, slot: &str, password: &str) -> SyncedSecret {
        SyncedSecret {
            id: SyncedSecret::id_for(slot),
            key: slot.into(),
            label: slot.into(),
            blob: key.encrypt(slot, password).unwrap(),
        }
    }

    #[test]
    fn records_from_a_vault_that_lost_are_resaved_or_dropped() {
        let salt = new_salt().unwrap();
        let loser = VaultKey::derive("loser passphrase", &salt).unwrap();
        let winner = VaultKey::derive("winner passphrase", &new_salt().unwrap()).unwrap();
        let mine = record(&loser, "ssh:a", "pw-a");
        let theirs = record(&loser, "ssh:b", "pw-b");
        assert!(
            winner.decrypt(&mine.key, &mine.blob).is_err(),
            "unreadable under the winning key"
        );

        let have = |slot: &str| (slot == "ssh:a").then(|| "pw-a".to_string());
        let (resaved, dropped) = repair_unreadable(&winner, &[mine.clone(), theirs.clone()], have);

        assert_eq!(resaved.len(), 1);
        assert_eq!(resaved[0].id, mine.id, "replaces the same record");
        assert_eq!(winner.decrypt("ssh:a", &resaved[0].blob).unwrap(), "pw-a");
        assert_eq!(dropped, vec![theirs.id], "no local copy: nothing can read it");
    }
}
