//! The folder each RDP connection shares with the remote computer.
//!
//! Kept in a file on this computer only, never in the connection library:
//! a path means nothing on another computer, and a setting that synced could
//! be used by another device to expose a folder here.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{err, AppState, CmdResult};

const FILE: &str = "rdp_shares.json";

/// Serializes the read-modify-write of the file.
static LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareDto {
    pub path: String,
    /// Whether the remote computer may change files; off unless chosen.
    #[serde(default)]
    pub writable: bool,
}

fn load(dir: &Path) -> BTreeMap<String, ShareDto> {
    fs::read_to_string(dir.join(FILE))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save(dir: &Path, all: &BTreeMap<String, ShareDto>) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{FILE}.tmp"));
    fs::write(&tmp, serde_json::to_string_pretty(all)?)?;
    fs::rename(tmp, dir.join(FILE))
}

/// The folder shared by a connection, if any.
pub fn get(dir: &Path, connection_id: &str) -> Option<ShareDto> {
    let _guard = LOCK.lock().ok()?;
    load(dir).remove(connection_id)
}

/// Share `share` with the connection, or stop sharing with `None`.
pub fn set(dir: &Path, connection_id: &str, share: Option<ShareDto>) -> Result<(), String> {
    let _guard = LOCK.lock().map_err(err)?;
    let mut all = load(dir);
    match share {
        Some(s) if !s.path.trim().is_empty() => {
            let path = Path::new(s.path.trim());
            if !path.is_absolute() {
                return Err("the shared folder must be a full path (starting with a drive letter on Windows)".into());
            }
            if !path.is_dir() {
                return Err(format!("{} is not a folder on this computer", path.display()));
            }
            all.insert(
                connection_id.to_string(),
                ShareDto {
                    path: path.to_string_lossy().into_owned(),
                    writable: s.writable,
                },
            );
        }
        _ => {
            all.remove(connection_id);
        }
    }
    save(dir, &all).map_err(err)
}

/// Whether this system can share a folder with a remote computer yet.
#[tauri::command]
pub fn rdp_share_supported() -> bool {
    cfg!(windows)
}

#[tauri::command]
pub fn get_rdp_share(state: State<AppState>, connection_id: String) -> CmdResult<Option<ShareDto>> {
    Ok(get(&state.data_dir, &connection_id))
}

#[tauri::command]
pub fn set_rdp_share(state: State<AppState>, connection_id: String, share: Option<ShareDto>) -> CmdResult<()> {
    set(&state.data_dir, &connection_id, share)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("linecard-shares-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_share_is_kept_per_connection_and_can_be_cleared() {
        let data = temp("data");
        let folder = temp("folder");
        let share = ShareDto {
            path: folder.to_string_lossy().into_owned(),
            writable: false,
        };
        assert_eq!(get(&data, "a"), None);
        set(&data, "a", Some(share.clone())).unwrap();
        assert_eq!(get(&data, "a"), Some(share.clone()));
        assert_eq!(get(&data, "b"), None, "other connections share nothing");
        set(
            &data,
            "a",
            Some(ShareDto {
                writable: true,
                ..share
            }),
        )
        .unwrap();
        assert!(get(&data, "a").unwrap().writable);
        set(&data, "a", None).unwrap();
        assert_eq!(get(&data, "a"), None);
        let _ = fs::remove_dir_all(&data);
        let _ = fs::remove_dir_all(&folder);
    }

    #[test]
    fn missing_or_relative_folders_are_refused() {
        let data = temp("refuse");
        let missing = data.join("nope");
        let share = |p: &str| ShareDto {
            path: p.into(),
            writable: false,
        };
        assert!(set(&data, "a", Some(share(&missing.to_string_lossy()))).is_err());
        assert!(set(&data, "a", Some(share("relative/folder"))).is_err());
        assert_eq!(get(&data, "a"), None);
        // An empty path clears, as the editor sends when the box is emptied.
        set(&data, "a", Some(share("  "))).unwrap();
        let _ = fs::remove_dir_all(&data);
    }

    #[test]
    fn a_missing_file_or_garbage_means_nothing_is_shared() {
        let data = temp("garbage");
        assert_eq!(get(&data, "a"), None);
        fs::write(data.join(FILE), "not json").unwrap();
        assert_eq!(get(&data, "a"), None);
        let _ = fs::remove_dir_all(&data);
    }
}
