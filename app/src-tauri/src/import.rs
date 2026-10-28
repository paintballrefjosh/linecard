//! Import from other clients: preview first, then add everything under one
//! new "Imported from …" folder (so it's easy to review, move or delete).

use std::collections::HashMap;
use std::path::PathBuf;

use linecard_core_model::{Connection, Folder, Settings, Tree};
use linecard_importers::{mremoteng, putty, securecrt, ssh_config, superputty, Import};
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSourceDto {
    pub id: &'static str,
    pub label: &'static str,
    /// Default location (None for the registry).
    pub path: Option<String>,
    /// Whether that default location exists on this computer.
    pub found: bool,
    pub hint: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRowDto {
    pub folder: String,
    pub name: String,
    pub kind: linecard_core_model::ConnectionKind,
    pub endpoint: String,
    pub username: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreviewDto {
    pub summary: String,
    pub warnings: Vec<String>,
    pub rows: Vec<ImportRowDto>,
}

fn env_path(var: &str, rest: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(|v| PathBuf::from(v).join(rest))
}

fn default_path(source: &str) -> Option<PathBuf> {
    match source {
        "securecrt" => env_path("APPDATA", r"VanDyke\Config\Sessions"),
        "mremoteng" => env_path("APPDATA", r"mRemoteNG\confCons.xml"),
        "ssh_config" => env_path("USERPROFILE", r".ssh\config").or_else(|| env_path("HOME", ".ssh/config")),
        _ => None,
    }
}

pub fn sources() -> Vec<ImportSourceDto> {
    let src = |id, label, hint| {
        let path = default_path(id);
        ImportSourceDto {
            id,
            label,
            found: match id {
                "putty" => putty::import_registry().is_ok_and(|i| !i.connections.is_empty()),
                _ => path.as_ref().is_some_and(|p| p.exists()),
            },
            path: path.map(|p| p.display().to_string()),
            hint,
        }
    };
    vec![
        src("securecrt", "SecureCRT", "The Config\\Sessions folder"),
        src("putty", "PuTTY", "Saved sessions are read from the registry"),
        src(
            "superputty",
            "SuperPuTTY",
            "Sessions.xml (folders come from the session paths)",
        ),
        src("mremoteng", "mRemoteNG", "confCons.xml (unencrypted)"),
        src("ssh_config", "OpenSSH config", "Usually ~/.ssh/config"),
    ]
}

pub fn load(source: &str, path: Option<&str>) -> Result<Import, String> {
    let path = path
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| default_path(source));
    let need = || {
        path.clone()
            .ok_or_else(|| "choose a file or folder to import".to_string())
    };
    let io = |e: std::io::Error| e.to_string();
    match source {
        "securecrt" => securecrt::import_dir(&need()?).map_err(io),
        "putty" => putty::import_registry().map_err(io),
        "mremoteng" => mremoteng::import_file(&need()?).map_err(io)?,
        "superputty" => superputty::import_file(&need()?).map_err(io)?,
        "ssh_config" => ssh_config::import_file(&need()?).map_err(io),
        other => Err(format!("unknown import source {other}")),
    }
}

pub fn preview(import: &Import) -> ImportPreviewDto {
    ImportPreviewDto {
        summary: import.summary(),
        warnings: import.warnings.clone(),
        rows: import
            .connections
            .iter()
            .map(|c| ImportRowDto {
                folder: c.folder_path.join(" / "),
                name: c.name.clone(),
                kind: c.kind,
                endpoint: c.endpoint.label(),
                username: c.username.clone(),
            })
            .collect(),
    }
}

/// Add the import to `tree` under a new top-level folder; returns its id.
/// Jump hosts named in the source are linked to the imported connection of
/// that name (or that host).
pub fn apply(tree: &mut Tree, import: &Import, new_id: impl Fn() -> String) -> Result<String, String> {
    let top = new_id();
    let order = tree.child_folders(None).iter().map(|f| f.order).fold(-1.0, f64::max) + 1.0;
    let folder = |id: String, name: String, parent: Option<String>, order: f64| Folder {
        id,
        name,
        parent,
        order,
        color: None,
        defaults: Settings::default(),
    };
    tree.add_folder(folder(
        top.clone(),
        format!("Imported from {}", import.source),
        None,
        order,
    ))
    .map_err(|e| e.to_string())?;

    let mut folders: HashMap<Vec<String>, String> = HashMap::new();
    let mut ensure = |tree: &mut Tree, path: &[String]| -> Result<String, String> {
        let mut parent = top.clone();
        for depth in 1..=path.len() {
            let key = path[..depth].to_vec();
            parent = match folders.get(&key) {
                Some(id) => id.clone(),
                None => {
                    let id = new_id();
                    tree.add_folder(folder(id.clone(), path[depth - 1].clone(), Some(parent), depth as f64))
                        .map_err(|e| e.to_string())?;
                    folders.insert(key, id.clone());
                    id
                }
            };
        }
        Ok(parent)
    };
    for path in &import.folders {
        ensure(tree, path)?;
    }

    let mut by_name: HashMap<String, String> = HashMap::new();
    let mut created = Vec::new();
    for (i, c) in import.connections.iter().enumerate() {
        let parent = ensure(tree, &c.folder_path)?;
        let id = new_id();
        tree.add_connection(Connection {
            id: id.clone(),
            name: c.name.clone(),
            folder: Some(parent),
            order: i as f64,
            kind: c.kind,
            endpoint: c.endpoint.clone(),
            overrides: Settings {
                username: c.username.clone(),
                ..Default::default()
            },
            notes: c.notes.clone(),
            tags: vec![],
        })
        .map_err(|e| e.to_string())?;
        by_name.entry(c.name.to_lowercase()).or_insert_with(|| id.clone());
        if let linecard_core_model::Endpoint::Network { host, .. } = &c.endpoint {
            by_name.entry(host.to_lowercase()).or_insert_with(|| id.clone());
        }
        created.push((id, c.jump_host.clone()));
    }
    for (id, jump) in created {
        if let Some(target) = jump.and_then(|j| by_name.get(&j.to_lowercase()).cloned()) {
            if target != id {
                tree.connection_mut(&id).map_err(|e| e.to_string())?.overrides.jump_host = Some(target);
            }
        }
    }
    Ok(top)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn applies_folders_users_and_jump_hosts() {
        let import =
            ssh_config::parse("Host bastion\n HostName 203.0.113.10\nHost core1\n User admin\n ProxyJump bastion\n");
        let mut tree = Tree::default();
        let n = Cell::new(0);
        let top = apply(&mut tree, &import, || {
            n.set(n.get() + 1);
            format!("id{}", n.get())
        })
        .unwrap();
        assert_eq!(tree.folder(&top).unwrap().name, "Imported from ssh_config");
        let core = tree.connections().find(|c| c.name == "core1").unwrap();
        let bastion = tree.connections().find(|c| c.name == "bastion").unwrap();
        assert_eq!(core.overrides.username.as_deref(), Some("admin"));
        assert_eq!(core.overrides.jump_host.as_deref(), Some(bastion.id.as_str()));
        assert_eq!(core.folder.as_deref(), Some(top.as_str()));
    }
}
