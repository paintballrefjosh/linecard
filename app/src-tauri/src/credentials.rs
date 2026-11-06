//! Named credentials: a user name and password that many connections share.
//! The name and user name sync with the library; the password goes to the
//! keychain slot logins already use (and to the synced vault when password
//! sync is on), so it is never sent to the window.

use linecard_core_model::{Credential, Tree};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::{edit, err, new_id, secrets, AppState, CmdResult};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialDto {
    pub id: String,
    pub name: String,
    pub username: String,
    /// A password is saved for it on this computer.
    pub saved: bool,
    /// Connections that log in with it (their own choice or inherited).
    pub used_by: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialDraft {
    /// Empty for a new credential.
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub username: String,
    /// A new password; `None` or empty keeps the saved one.
    #[serde(default)]
    pub password: Option<String>,
}

/// What Settings shows for a credential's saved password.
fn label(c: &Credential) -> String {
    if c.username.is_empty() {
        c.name.clone()
    } else {
        format!("{} ({})", c.name, c.username)
    }
}

/// How many connections resolve to each credential.
fn usage(tree: &Tree) -> std::collections::HashMap<String, usize> {
    let mut out = std::collections::HashMap::new();
    for c in tree.connections() {
        if let Some(id) = tree.resolve(&c.id).ok().and_then(|r| r.settings.credential_id) {
            *out.entry(id).or_default() += 1;
        }
    }
    out
}

/// Every credential, by name.
#[tauri::command]
pub fn list_credentials(state: State<AppState>) -> CmdResult<Vec<CredentialDto>> {
    let lib = state.lib.lock().map_err(err)?;
    let tree = lib.tree();
    let used = usage(tree);
    let mut out: Vec<CredentialDto> = tree
        .credentials
        .values()
        .map(|c| CredentialDto {
            id: c.id.clone(),
            name: c.name.clone(),
            username: c.username.clone(),
            saved: state.vault.get(&Credential::slot(&c.id)).is_some(),
            used_by: used.get(&c.id).copied().unwrap_or(0),
        })
        .collect();
    out.sort_by_key(|c| c.name.to_lowercase());
    Ok(out)
}

/// Create (empty id) or update a credential, and save its password if one
/// was typed. Returns its id.
#[tauri::command]
pub fn save_credential(app: AppHandle, state: State<AppState>, draft: CredentialDraft) -> CmdResult<String> {
    let name = draft.name.trim().to_string();
    if name.is_empty() {
        return Err("Name the credential".into());
    }
    let cred = Credential {
        id: if draft.id.is_empty() { new_id() } else { draft.id },
        name,
        username: draft.username.trim().to_string(),
    };
    let password = draft.password.filter(|p| !p.is_empty());
    let store = state.ssh.credentials.clone();
    if password.is_some() && store.is_none() {
        return Err("passwords can't be saved here".into());
    }
    let id = cred.id.clone();
    let slot = Credential::slot(&id);
    let label = label(&cred);
    edit(&app, &state, |t| {
        t.credentials.insert(cred.id.clone(), cred);
        Ok(())
    })?;
    if let (Some(p), Some(store)) = (password, store) {
        store.save_password(&slot, &label, &p);
    }
    Ok(id)
}

/// Delete a credential: connections and folders that used it go back to
/// their own user name and saved password, and its password is forgotten.
#[tauri::command]
pub fn delete_credential(app: AppHandle, state: State<AppState>, id: String) -> CmdResult<()> {
    edit(&app, &state, |t| {
        t.credentials.remove(&id);
        clear_references(t, &id).map_err(err)
    })?;
    let slot = Credential::slot(&id);
    if state.vault.get(&slot).is_some() {
        state.vault.forget(&slot).map_err(err)?;
        secrets::after_local_forget(&app, &slot);
    }
    Ok(())
}

fn clear_references(t: &mut Tree, id: &str) -> Result<(), linecard_core_model::TreeError> {
    let is = |s: &Option<String>| s.as_deref() == Some(id);
    if is(&t.global.credential_id) {
        t.global.credential_id = None;
    }
    let folders: Vec<String> = t
        .folders()
        .filter(|f| is(&f.defaults.credential_id))
        .map(|f| f.id.clone())
        .collect();
    for f in folders {
        t.folder_mut(&f)?.defaults.credential_id = None;
    }
    let conns: Vec<String> = t
        .connections()
        .filter(|c| is(&c.overrides.credential_id))
        .map(|c| c.id.clone())
        .collect();
    for c in conns {
        t.connection_mut(&c)?.overrides.credential_id = None;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use linecard_core_model::{Connection, ConnectionKind, Endpoint, Folder, Settings};

    #[test]
    fn deleting_a_credential_clears_every_reference_and_usage_follows_inheritance() {
        let mut t = Tree::default();
        t.credentials.insert(
            "c1".into(),
            Credential {
                id: "c1".into(),
                name: "NetOps".into(),
                username: "admin".into(),
            },
        );
        t.add_folder(Folder {
            id: "dc".into(),
            name: "DC".into(),
            parent: None,
            order: 0.0,
            color: None,
            defaults: Settings {
                credential_id: Some("c1".into()),
                ..Default::default()
            },
        })
        .unwrap();
        for (id, own) in [("a", None), ("b", Some("c1"))] {
            t.add_connection(Connection {
                id: id.into(),
                name: id.into(),
                folder: Some("dc".into()),
                order: 0.0,
                kind: ConnectionKind::Ssh,
                endpoint: Endpoint::network(ConnectionKind::Ssh, "h"),
                overrides: Settings {
                    credential_id: own.map(Into::into),
                    ..Default::default()
                },
                notes: String::new(),
                tags: vec![],
                forwards: vec![],
            })
            .unwrap();
        }
        assert_eq!(usage(&t).get("c1"), Some(&2));
        clear_references(&mut t, "c1").unwrap();
        assert!(usage(&t).is_empty());
        assert!(t.folder("dc").unwrap().defaults.credential_id.is_none());
    }
}
