use std::collections::{BTreeMap, HashSet};
use std::fmt;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{ConnectionKind, Endpoint, ObjectId, Resolved, Settings, Source};

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Folder {
    pub id: ObjectId,
    pub name: String,
    /// `None` = top level.
    pub parent: Option<ObjectId>,
    /// Fractional sort key so a move between two siblings touches one object.
    pub order: f64,
    /// Lucide icon colour / accent chosen by the user.
    pub color: Option<String>,
    /// Defaults inherited by everything below this folder.
    pub defaults: Settings,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Connection {
    pub id: ObjectId,
    pub name: String,
    /// `None` = top level.
    pub folder: Option<ObjectId>,
    pub order: f64,
    pub kind: ConnectionKind,
    pub endpoint: Endpoint,
    /// Values set on this connection; anything `None` is inherited.
    pub overrides: Settings,
    pub notes: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeError {
    NotFound(ObjectId),
    /// Moving a folder into itself or one of its descendants.
    Cycle {
        folder: ObjectId,
        target: ObjectId,
    },
    DuplicateId(ObjectId),
}

impl fmt::Display for TreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TreeError::NotFound(id) => write!(f, "object {id} not found"),
            TreeError::Cycle { folder, target } => {
                write!(f, "moving folder {folder} into {target} would create a cycle")
            }
            TreeError::DuplicateId(id) => write!(f, "object {id} already exists"),
        }
    }
}

impl std::error::Error for TreeError {}

/// In-memory connection tree. The store and sync layers persist and replicate
/// it; this type owns the rules (inheritance, cycle checks, paths).
#[derive(Debug, Clone, Default)]
pub struct Tree {
    pub global: Settings,
    folders: BTreeMap<ObjectId, Folder>,
    connections: BTreeMap<ObjectId, Connection>,
}

impl Tree {
    pub fn new(global: Settings) -> Self {
        Tree {
            global,
            ..Default::default()
        }
    }

    pub fn add_folder(&mut self, folder: Folder) -> Result<(), TreeError> {
        self.check_new_id(&folder.id)?;
        if let Some(p) = &folder.parent {
            self.folder(p)?;
        }
        self.folders.insert(folder.id.clone(), folder);
        Ok(())
    }

    pub fn add_connection(&mut self, conn: Connection) -> Result<(), TreeError> {
        self.check_new_id(&conn.id)?;
        if let Some(f) = &conn.folder {
            self.folder(f)?;
        }
        self.connections.insert(conn.id.clone(), conn);
        Ok(())
    }

    fn check_new_id(&self, id: &ObjectId) -> Result<(), TreeError> {
        if self.folders.contains_key(id) || self.connections.contains_key(id) {
            Err(TreeError::DuplicateId(id.clone()))
        } else {
            Ok(())
        }
    }

    pub fn folder(&self, id: &str) -> Result<&Folder, TreeError> {
        self.folders.get(id).ok_or_else(|| TreeError::NotFound(id.to_string()))
    }

    pub fn connection(&self, id: &str) -> Result<&Connection, TreeError> {
        self.connections
            .get(id)
            .ok_or_else(|| TreeError::NotFound(id.to_string()))
    }

    pub fn folder_mut(&mut self, id: &str) -> Result<&mut Folder, TreeError> {
        self.folders
            .get_mut(id)
            .ok_or_else(|| TreeError::NotFound(id.to_string()))
    }

    pub fn connection_mut(&mut self, id: &str) -> Result<&mut Connection, TreeError> {
        self.connections
            .get_mut(id)
            .ok_or_else(|| TreeError::NotFound(id.to_string()))
    }

    pub fn folders(&self) -> impl Iterator<Item = &Folder> {
        self.folders.values()
    }

    pub fn connections(&self) -> impl Iterator<Item = &Connection> {
        self.connections.values()
    }

    /// Child folders of `parent` (`None` = top level), sorted by `order` then name.
    pub fn child_folders(&self, parent: Option<&str>) -> Vec<&Folder> {
        let mut v: Vec<_> = self
            .folders
            .values()
            .filter(|f| f.parent.as_deref() == parent)
            .collect();
        v.sort_by(|a, b| a.order.total_cmp(&b.order).then_with(|| a.name.cmp(&b.name)));
        v
    }

    /// Connections directly in `folder`, sorted by `order` then name.
    pub fn child_connections(&self, folder: Option<&str>) -> Vec<&Connection> {
        let mut v: Vec<_> = self
            .connections
            .values()
            .filter(|c| c.folder.as_deref() == folder)
            .collect();
        v.sort_by(|a, b| a.order.total_cmp(&b.order).then_with(|| a.name.cmp(&b.name)));
        v
    }

    /// Folder ids from the top level down to `folder` inclusive.
    pub fn ancestry(&self, folder: &str) -> Result<Vec<ObjectId>, TreeError> {
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut cur = Some(folder.to_string());
        while let Some(id) = cur {
            if !seen.insert(id.clone()) {
                // Corrupt data (a cycle that slipped in via sync): stop safely.
                break;
            }
            let f = self.folder(&id)?;
            cur = f.parent.clone();
            chain.push(id);
        }
        chain.reverse();
        Ok(chain)
    }

    /// Display path such as `/Datacenter/Spine`.
    pub fn path(&self, folder: Option<&str>) -> Result<String, TreeError> {
        let Some(folder) = folder else { return Ok("/".into()) };
        let names = self
            .ancestry(folder)?
            .iter()
            .map(|id| self.folder(id).map(|f| f.name.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(format!("/{}", names.join("/")))
    }

    /// True if `candidate` is `folder` or lies below it.
    pub fn is_within(&self, candidate: &str, folder: &str) -> bool {
        self.ancestry(candidate)
            .map(|chain| chain.iter().any(|id| id == folder))
            .unwrap_or(false)
    }

    /// Move a folder under `new_parent`, rejecting moves that create a cycle.
    pub fn move_folder(&mut self, id: &str, new_parent: Option<&str>) -> Result<(), TreeError> {
        self.folder(id)?;
        if let Some(target) = new_parent {
            self.folder(target)?;
            if self.is_within(target, id) {
                return Err(TreeError::Cycle {
                    folder: id.into(),
                    target: target.into(),
                });
            }
        }
        self.folder_mut(id)?.parent = new_parent.map(str::to_string);
        Ok(())
    }

    pub fn move_connection(&mut self, id: &str, folder: Option<&str>) -> Result<(), TreeError> {
        if let Some(f) = folder {
            self.folder(f)?;
        }
        self.connection_mut(id)?.folder = folder.map(str::to_string);
        Ok(())
    }

    /// Remove a folder and everything below it. Returns removed ids
    /// (folders and connections) so the sync layer can tombstone them.
    pub fn remove_folder(&mut self, id: &str) -> Result<Vec<ObjectId>, TreeError> {
        self.folder(id)?;
        let doomed_folders: Vec<ObjectId> = self.folders.keys().filter(|f| self.is_within(f, id)).cloned().collect();
        let doomed_conns: Vec<ObjectId> = self
            .connections
            .values()
            .filter(|c| {
                c.folder
                    .as_deref()
                    .is_some_and(|f| doomed_folders.iter().any(|d| d == f))
            })
            .map(|c| c.id.clone())
            .collect();
        for c in &doomed_conns {
            self.connections.remove(c);
        }
        for f in &doomed_folders {
            self.folders.remove(f);
        }
        Ok(doomed_conns.into_iter().chain(doomed_folders).collect())
    }

    pub fn remove_connection(&mut self, id: &str) -> Result<Connection, TreeError> {
        self.connections
            .remove(id)
            .ok_or_else(|| TreeError::NotFound(id.to_string()))
    }

    /// Effective settings for a connection: own overrides, then the nearest
    /// folder upward, then global defaults.
    pub fn resolve(&self, connection: &str) -> Result<Resolved, TreeError> {
        let conn = self.connection(connection)?;
        let mut out = Resolved::default();
        out.settings.fill_from(&conn.overrides, &Source::Own, &mut out.sources);
        if let Some(folder) = &conn.folder {
            for fid in self.ancestry(folder)?.iter().rev() {
                let f = self.folder(fid)?;
                out.settings
                    .fill_from(&f.defaults, &Source::Folder(fid.clone()), &mut out.sources);
            }
        }
        out.settings.fill_from(&self.global, &Source::Global, &mut out.sources);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SerialSettings;

    fn folder(id: &str, parent: Option<&str>, defaults: Settings) -> Folder {
        Folder {
            id: id.into(),
            name: id.to_uppercase(),
            parent: parent.map(Into::into),
            order: 0.0,
            color: None,
            defaults,
        }
    }

    fn conn(id: &str, folder: Option<&str>, overrides: Settings) -> Connection {
        Connection {
            id: id.into(),
            name: id.into(),
            folder: folder.map(Into::into),
            order: 0.0,
            kind: ConnectionKind::Ssh,
            endpoint: Endpoint::network(ConnectionKind::Ssh, format!("{id}.lab")),
            overrides,
            notes: String::new(),
            tags: vec![],
        }
    }

    fn sample() -> Tree {
        let mut t = Tree::new(Settings {
            username: Some("global-user".into()),
            keepalive_secs: Some(30),
            log_enabled: Some(false),
            ..Default::default()
        });
        t.add_folder(folder(
            "dc",
            None,
            Settings {
                username: Some("netadmin".into()),
                log_enabled: Some(true),
                ..Default::default()
            },
        ))
        .unwrap();
        t.add_folder(folder(
            "spine",
            Some("dc"),
            Settings {
                jump_host: Some("bastion".into()),
                tab_color: Some("#d64545".into()),
                ..Default::default()
            },
        ))
        .unwrap();
        t.add_connection(conn(
            "spine01",
            Some("spine"),
            Settings {
                tab_color: Some("#3366ff".into()),
                ..Default::default()
            },
        ))
        .unwrap();
        t
    }

    #[test]
    fn resolve_walks_up_the_tree() {
        let t = sample();
        let r = t.resolve("spine01").unwrap();
        assert_eq!(r.settings.tab_color.as_deref(), Some("#3366ff"));
        assert_eq!(r.source_of("tab_color"), Some(&Source::Own));
        assert_eq!(r.settings.jump_host.as_deref(), Some("bastion"));
        assert_eq!(r.source_of("jump_host"), Some(&Source::Folder("spine".into())));
        // nearest folder wins over global
        assert_eq!(r.settings.username.as_deref(), Some("netadmin"));
        assert_eq!(r.source_of("username"), Some(&Source::Folder("dc".into())));
        assert_eq!(r.settings.log_enabled, Some(true));
        assert_eq!(r.settings.keepalive_secs, Some(30));
        assert_eq!(r.source_of("keepalive_secs"), Some(&Source::Global));
        assert_eq!(r.source_of("credential_id"), None);
    }

    #[test]
    fn paths_and_children() {
        let mut t = sample();
        assert_eq!(t.path(Some("spine")).unwrap(), "/DC/SPINE");
        assert_eq!(t.path(None).unwrap(), "/");
        t.add_folder(folder("leaf", Some("dc"), Settings::default())).unwrap();
        let kids: Vec<_> = t.child_folders(Some("dc")).iter().map(|f| f.id.clone()).collect();
        assert_eq!(kids, vec!["leaf", "spine"]);
        assert_eq!(t.child_connections(Some("spine")).len(), 1);
    }

    #[test]
    fn move_rejects_cycles() {
        let mut t = sample();
        assert_eq!(
            t.move_folder("dc", Some("spine")),
            Err(TreeError::Cycle {
                folder: "dc".into(),
                target: "spine".into()
            })
        );
        assert!(t.move_folder("dc", Some("dc")).is_err());
        t.move_folder("spine", None).unwrap();
        assert_eq!(t.path(Some("spine")).unwrap(), "/SPINE");
        // after the move spine01 no longer inherits from dc
        assert_eq!(
            t.resolve("spine01").unwrap().settings.username.as_deref(),
            Some("global-user")
        );
    }

    #[test]
    fn remove_folder_cascades() {
        let mut t = sample();
        let mut serial = conn("con1", Some("dc"), Settings::default());
        serial.kind = ConnectionKind::Serial;
        serial.endpoint = Endpoint::Serial(SerialSettings::console("COM3"));
        t.add_connection(serial).unwrap();
        let mut removed = t.remove_folder("dc").unwrap();
        removed.sort();
        assert_eq!(removed, vec!["con1", "dc", "spine", "spine01"]);
        assert_eq!(t.folders().count(), 0);
        assert_eq!(t.connections().count(), 0);
    }

    #[test]
    fn rejects_duplicates_and_missing_parents() {
        let mut t = sample();
        assert_eq!(
            t.add_folder(folder("dc", None, Settings::default())),
            Err(TreeError::DuplicateId("dc".into()))
        );
        assert!(t.add_connection(conn("x", Some("nope"), Settings::default())).is_err());
    }
}
