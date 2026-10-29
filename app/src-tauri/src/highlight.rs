//! Keyword-highlight profiles: commands, starter profile, SecureCRT import.
//!
//! Profiles sync like folders. A folder or connection picks one through the
//! inheritable `highlight_rules` setting; with nothing picked anywhere, the
//! app-wide default applies (Settings), else the oldest profile.

use linecard_core_model::{HighlightProfile, HighlightRule, Tree};
use linecard_importers::securecrt_keywords;
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::library::Library;
use crate::{edit, err, new_id, AppState, CmdResult};

/// Set once this install has a profile (or was migrated from the old
/// per-device keyword list).
const MIGRATED_KEY: &str = "highlight_migrated";

/// Network-gear defaults: problems red, attention amber, healthy green.
/// Keep in step with `defaultRules` in app/src/highlight.ts.
pub fn default_rules() -> Vec<HighlightRule> {
    [
        ("err-disabled", "red"),
        ("down", "red"),
        ("error", "red"),
        ("errors", "red"),
        ("failed", "red"),
        ("denied", "red"),
        ("notconnect", "amber"),
        ("warning", "amber"),
        ("flapping", "amber"),
        ("up", "green"),
        ("connected", "green"),
        ("established", "green"),
    ]
    .into_iter()
    .map(|(text, color)| HighlightRule {
        text: text.into(),
        color: color.into(),
        regex: false,
    })
    .collect()
}

fn general(rules: Vec<HighlightRule>) -> HighlightProfile {
    HighlightProfile {
        id: new_id(),
        name: "General".into(),
        match_case: false,
        rules,
    }
}

/// First-run starter profile.
pub fn seed(tree: &mut Tree) {
    let p = general(default_rules());
    tree.highlight_profiles.insert(p.id.clone(), p);
}

/// Installs from before profiles kept one keyword list in this device's
/// preferences; turn it into a "General" profile once.
pub fn migrate(lib: &mut Library, preferences_key: &str) -> Result<(), String> {
    if lib.local_setting(MIGRATED_KEY)?.is_some() {
        return Ok(());
    }
    if lib.tree().highlight_profiles.is_empty() {
        let rules = lib
            .local_setting(preferences_key)?
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|p| serde_json::from_value::<Vec<HighlightRule>>(p.get("highlightRules")?.clone()).ok())
            .unwrap_or_else(default_rules);
        lib.edit(|t| {
            let p = general(rules);
            t.highlight_profiles.insert(p.id.clone(), p);
            Ok(())
        })?;
    }
    lib.set_local_setting(MIGRATED_KEY, "1")
}

/// The profile that applies when nothing more specific is picked.
pub fn default_id(tree: &Tree) -> Option<String> {
    tree.global
        .highlight_rules
        .clone()
        .filter(|id| tree.highlight_profiles.contains_key(id))
        .or_else(|| tree.highlight_profiles.keys().min().cloned())
}

/// The profile for a saved connection (or the default for quick connects).
pub fn effective(tree: &Tree, connection: Option<&str>) -> Option<String> {
    connection
        .and_then(|c| tree.resolve(c).ok())
        .and_then(|r| r.settings.highlight_rules)
        .filter(|id| tree.highlight_profiles.contains_key(id))
        .or_else(|| default_id(tree))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HighlightDto {
    /// Sorted by name.
    pub profiles: Vec<HighlightProfile>,
    pub default_id: Option<String>,
}

#[tauri::command]
pub fn get_highlight(state: State<AppState>) -> CmdResult<HighlightDto> {
    let lib = state.lib.lock().map_err(err)?;
    let tree = lib.tree();
    let mut profiles: Vec<HighlightProfile> = tree.highlight_profiles.values().cloned().collect();
    profiles.sort_by_key(|p| p.name.to_lowercase());
    Ok(HighlightDto {
        profiles,
        default_id: default_id(tree),
    })
}

#[tauri::command]
pub fn effective_highlight(state: State<AppState>, connection_id: Option<String>) -> CmdResult<Option<String>> {
    let lib = state.lib.lock().map_err(err)?;
    Ok(effective(lib.tree(), connection_id.as_deref()))
}

/// Create (empty id) or replace a profile. Returns its id.
#[tauri::command]
pub fn save_highlight_profile(
    app: AppHandle,
    state: State<AppState>,
    mut profile: HighlightProfile,
) -> CmdResult<String> {
    profile.name = profile.name.trim().to_string();
    if profile.name.is_empty() {
        return Err("Name the profile".into());
    }
    if profile.id.is_empty() {
        profile.id = new_id();
    }
    profile.rules.retain(|r| !r.text.trim().is_empty());
    let id = profile.id.clone();
    edit(&app, &state, |t| {
        t.highlight_profiles.insert(profile.id.clone(), profile);
        Ok(())
    })?;
    Ok(id)
}

/// Delete a profile; folders and connections that used it fall back to the
/// default.
#[tauri::command]
pub fn delete_highlight_profile(app: AppHandle, state: State<AppState>, id: String) -> CmdResult<()> {
    edit(&app, &state, |t| {
        if t.highlight_profiles.len() <= 1 {
            return Err("Keep at least one profile".into());
        }
        t.highlight_profiles.remove(&id);
        if t.global.highlight_rules.as_ref() == Some(&id) {
            t.global.highlight_rules = None;
        }
        let folders: Vec<String> = t
            .folders()
            .filter(|f| f.defaults.highlight_rules.as_ref() == Some(&id))
            .map(|f| f.id.clone())
            .collect();
        for f in folders {
            t.folder_mut(&f).map_err(err)?.defaults.highlight_rules = None;
        }
        let conns: Vec<String> = t
            .connections()
            .filter(|c| c.overrides.highlight_rules.as_ref() == Some(&id))
            .map(|c| c.id.clone())
            .collect();
        for c in conns {
            t.connection_mut(&c).map_err(err)?.overrides.highlight_rules = None;
        }
        Ok(())
    })
}

#[tauri::command]
pub fn set_default_highlight(app: AppHandle, state: State<AppState>, id: String) -> CmdResult<()> {
    edit(&app, &state, |t| {
        if !t.highlight_profiles.contains_key(&id) {
            return Err("That profile no longer exists".into());
        }
        t.global.highlight_rules = Some(id);
        Ok(())
    })
}

#[derive(Serialize)]
pub struct KeywordFileDto {
    pub name: String,
    pub path: String,
}

/// SecureCRT keyword lists found on this computer.
#[tauri::command]
pub fn securecrt_keyword_files() -> Vec<KeywordFileDto> {
    securecrt_keywords::default_dir()
        .map(|d| securecrt_keywords::list(&d))
        .unwrap_or_default()
        .into_iter()
        .map(|p| KeywordFileDto {
            name: p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: p.display().to_string(),
        })
        .collect()
}

/// Import a SecureCRT keyword list as a new profile. Returns its id.
#[tauri::command]
pub fn import_highlight(app: AppHandle, state: State<AppState>, path: String) -> CmdResult<String> {
    let path = std::path::PathBuf::from(path);
    let text = std::fs::read(&path).map_err(err)?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut profile = securecrt_keywords::parse(&String::from_utf8_lossy(&text), &stem)?;
    {
        // "Cisco Words" twice becomes "Cisco Words (2)".
        let lib = state.lib.lock().map_err(err)?;
        let taken = |n: &str| {
            lib.tree()
                .highlight_profiles
                .values()
                .any(|p| p.name.eq_ignore_ascii_case(n))
        };
        if taken(&profile.name) {
            let base = profile.name.clone();
            profile.name = (2..)
                .map(|i| format!("{base} ({i})"))
                .find(|n| !taken(n))
                .unwrap_or(base);
        }
    }
    save_highlight_profile(app, state, profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use linecard_core_model::{Connection, ConnectionKind, Endpoint, Folder, Settings};

    #[test]
    fn profiles_persist_and_resolve_through_folders() {
        let mut lib = Library::open_in_memory(seed).unwrap();
        let general = default_id(lib.tree()).expect("starter profile");
        lib.edit(|t| {
            let mut cisco = general_profile_named("Cisco");
            cisco.id = "cisco".into();
            t.highlight_profiles.insert(cisco.id.clone(), cisco);
            t.add_folder(Folder {
                id: "dc".into(),
                name: "DC".into(),
                parent: None,
                order: 0.0,
                color: None,
                defaults: Settings {
                    highlight_rules: Some("cisco".into()),
                    ..Default::default()
                },
            })
            .map_err(err)?;
            t.add_connection(Connection {
                id: "sw1".into(),
                name: "sw1".into(),
                folder: Some("dc".into()),
                order: 0.0,
                kind: ConnectionKind::Ssh,
                endpoint: Endpoint::network(ConnectionKind::Ssh, "sw1"),
                overrides: Settings::default(),
                notes: String::new(),
                tags: vec![],
                forwards: vec![],
            })
            .map_err(err)
        })
        .unwrap();
        let t = lib.tree();
        assert_eq!(
            t.highlight_profiles["cisco"].rules,
            default_rules(),
            "rules survive the op log"
        );
        assert_eq!(
            effective(t, Some("sw1")).as_deref(),
            Some("cisco"),
            "inherited from the folder"
        );
        assert_eq!(
            effective(t, None),
            Some(general.clone()),
            "quick connects use the default"
        );

        // Deleting the profile clears the folder's pick.
        lib.edit(|t| {
            t.highlight_profiles.remove("cisco");
            t.folder_mut("dc").map_err(err)?.defaults.highlight_rules = None;
            Ok(())
        })
        .unwrap();
        assert_eq!(effective(lib.tree(), Some("sw1")), Some(general));
    }

    fn general_profile_named(name: &str) -> HighlightProfile {
        HighlightProfile {
            name: name.into(),
            ..general(default_rules())
        }
    }
}
