//! The user's connection library: the synced replica, persisted in SQLite,
//! with the folder/connection [`Tree`] projected from it.
//!
//! The replica (an op log) is the source of truth. Commands edit a copy of
//! the tree with its usual rules (cycle checks, inheritance); [`Library::edit`]
//! diffs that copy against the current tree and records each changed object
//! as an op — create, per-field update, or delete — so every edit is
//! persisted, revertible and ready to sync.
//!
//! Inheritable settings are stored one field per setting (`defaults.username`,
//! `overrides.tab_color`, …) so concurrent edits to different settings of the
//! same object merge instead of overwriting each other.

use std::collections::BTreeMap;
use std::path::Path;

use linecard_core_model::{Connection, ConnectionKind, Endpoint, Folder, HighlightProfile, Settings, Tree};
use linecard_core_store::Store;
use linecard_core_sync::{ChangeTracker, Hlc, ObjectState, Op, Replica, Value};

pub type Result<T> = std::result::Result<T, String>;

const DEVICE_KEY: &str = "device_id";
const SEEDED_KEY: &str = "seeded";
const SEED_UNTIL_KEY: &str = "seed_until";
/// Object id of the app-wide default settings.
const GLOBAL_ID: &str = "global";

const KIND_FOLDER: &str = "folder";
const KIND_CONNECTION: &str = "connection";
const KIND_SETTINGS: &str = "settings";
const KIND_HIGHLIGHT: &str = "highlight_profile";

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub struct Library {
    store: Store,
    replica: Replica,
    tree: Tree,
}

impl Library {
    /// Open (or create) the library at `path`. `seed` fills a brand-new
    /// library with starter content.
    pub fn open(path: &Path, seed: impl FnOnce(&mut Tree)) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(err)?;
        }
        Self::init(Store::open(path).map_err(err)?, seed)
    }

    #[cfg(test)]
    pub fn open_in_memory(seed: impl FnOnce(&mut Tree)) -> Result<Self> {
        Self::init(Store::open_in_memory().map_err(err)?, seed)
    }

    fn init(store: Store, seed: impl FnOnce(&mut Tree)) -> Result<Self> {
        let device = match store.setting(DEVICE_KEY).map_err(err)? {
            Some(d) => d,
            None => {
                let d = uuid::Uuid::now_v7().to_string();
                store.set_setting(DEVICE_KEY, &d).map_err(err)?;
                d
            }
        };
        let replica = store.load_replica(&device, now_ms()).map_err(err)?;
        let tree = project(&replica);
        let mut lib = Library { store, replica, tree };
        if lib.store.setting(SEEDED_KEY).map_err(err)?.is_none() {
            lib.edit(|t| {
                seed(t);
                Ok(())
            })?;
            lib.store.set_setting(SEEDED_KEY, "1").map_err(err)?;
            // Remember where the starter content ends, so joining a sync
            // group from an untouched install doesn't push another copy.
            if let Some(last) = lib.last_own_op() {
                lib.store
                    .set_setting(SEED_UNTIL_KEY, &serde_json::to_string(&last).map_err(err)?)
                    .map_err(err)?;
            }
        }
        Ok(lib)
    }

    // ---- sync ----------------------------------------------------------------

    /// This device's newest op.
    pub fn last_own_op(&self) -> Option<Hlc> {
        let me = self.device();
        self.replica
            .log()
            .iter()
            .filter(|o| o.id.device == me)
            .map(|o| o.id.clone())
            .max()
    }

    /// The last op of the starter content, if this install seeded it.
    pub fn seeded_until(&self) -> Option<Hlc> {
        self.local_setting(SEED_UNTIL_KEY)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    /// Joining a sync group: an install nobody has touched yet drops its
    /// starter content (the group has its own). Returns the op to treat as
    /// already pushed, so neither the starter nor its removal is sent;
    /// `None` if the user has made changes (everything gets pushed).
    pub fn drop_untouched_seed(&mut self) -> Result<Option<Hlc>> {
        let seed = self.seeded_until();
        if seed.is_none() || self.last_own_op() != seed {
            return Ok(None);
        }
        self.edit(|t| {
            let roots: Vec<String> = t.child_folders(None).iter().map(|f| f.id.clone()).collect();
            for id in roots {
                t.remove_folder(&id).map_err(err)?;
            }
            let loose: Vec<String> = t.child_connections(None).iter().map(|c| c.id.clone()).collect();
            for id in loose {
                t.remove_connection(&id).map_err(err)?;
            }
            t.highlight_profiles.clear();
            Ok(())
        })?;
        Ok(self.last_own_op())
    }

    /// Ops made on this device after `after` (all of them for `None`),
    /// oldest first: what still needs pushing.
    pub fn own_ops_after(&self, after: Option<&Hlc>) -> Vec<Op> {
        let me = self.device();
        let mut ops: Vec<Op> = self
            .replica
            .log()
            .iter()
            .filter(|o| o.id.device == me && after.is_none_or(|a| o.id > *a))
            .cloned()
            .collect();
        ops.sort_by(|a, b| a.id.cmp(&b.id));
        ops
    }

    /// Merge ops from other devices. Returns how many were new; those are
    /// stored and show up as highlighted changes.
    pub fn apply_remote(&mut self, ops: Vec<Op>) -> Result<usize> {
        let now = now_ms();
        let fresh: Vec<Op> = ops
            .into_iter()
            .filter(|op| self.replica.apply_remote(op.clone(), now))
            .collect();
        if fresh.is_empty() {
            return Ok(0);
        }
        self.store.append_ops(&fresh).map_err(err)?;
        self.tree = project(&self.replica);
        Ok(fresh.len())
    }

    pub fn sync_cursor(&self, name: &str) -> Result<Option<String>> {
        self.store.cursor(name).map_err(err)
    }

    pub fn set_sync_cursor(&self, name: &str, value: &str) -> Result<()> {
        self.store.set_cursor(name, value).map_err(err)
    }

    pub fn device(&self) -> &str {
        self.replica.device()
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// Per-device value that never syncs (UI preferences, window state).
    pub fn local_setting(&self, key: &str) -> Result<Option<String>> {
        self.store.setting(key).map_err(err)
    }

    pub fn set_local_setting(&self, key: &str, value: &str) -> Result<()> {
        self.store.set_setting(key, value).map_err(err)
    }

    /// Remote changes awaiting acknowledgement on this device.
    pub fn changes(&self) -> &ChangeTracker {
        &self.replica.changes
    }

    pub fn changes_mut(&mut self) -> &mut ChangeTracker {
        &mut self.replica.changes
    }

    /// Apply `f` to a copy of the tree and record what it changed. Nothing is
    /// written if `f` fails.
    pub fn edit<T>(&mut self, f: impl FnOnce(&mut Tree) -> Result<T>) -> Result<T> {
        let mut next = self.tree.clone();
        let out = f(&mut next)?;
        let now = now_ms();
        // Only objects the replica holds count as existing (the global
        // settings object is implicit until first written).
        let mut before = snapshot(&self.tree);
        before.retain(|id, _| self.replica.object(id).is_some_and(|o| !o.is_deleted()));
        let after = snapshot(&next);
        let mut ops = Vec::new();
        for (id, (kind, fields)) in &after {
            match before.get(id) {
                None => ops.push(self.replica.create(id, kind, fields.clone(), now)),
                Some((_, old)) => {
                    let changed: BTreeMap<String, Value> = fields
                        .iter()
                        .filter(|(k, v)| old.get(*k) != Some(*v))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    if !changed.is_empty() {
                        ops.push(self.replica.update(id, changed, now).map_err(err)?);
                    }
                }
            }
        }
        for id in before.keys().filter(|id| !after.contains_key(*id)) {
            ops.push(self.replica.delete(id, now).map_err(err)?);
        }
        self.commit(&ops)?;
        Ok(out)
    }

    /// Undo one op (local or remote) with a new op.
    pub fn revert(&mut self, op: &Hlc) -> Result<()> {
        let op = self.replica.revert(op, now_ms()).map_err(err)?;
        self.commit(&[op])
    }

    fn commit(&mut self, ops: &[Op]) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        self.store.append_ops(ops).map_err(err)?;
        self.tree = project(&self.replica);
        Ok(())
    }
}

// ---- tree -> fields ------------------------------------------------------------

type Fields = BTreeMap<String, Value>;

fn opt_str(s: &Option<String>) -> Value {
    s.as_ref().map_or(Value::Null, |s| Value::Str(s.clone()))
}

fn json(v: &impl serde::Serialize) -> Value {
    Value::Str(serde_json::to_string(v).unwrap_or_default())
}

/// One field per inheritable setting, JSON-encoded; unset is Null.
fn settings_fields(prefix: &str, s: &Settings, out: &mut Fields) {
    let obj = serde_json::to_value(s).unwrap_or_default();
    for name in Settings::FIELDS {
        let v = match obj.get(*name) {
            None | Some(serde_json::Value::Null) => Value::Null,
            Some(v) => Value::Str(v.to_string()),
        };
        out.insert(format!("{prefix}.{name}"), v);
    }
}

fn folder_fields(f: &Folder) -> Fields {
    let mut m = Fields::new();
    m.insert("name".into(), Value::Str(f.name.clone()));
    m.insert("parent".into(), opt_str(&f.parent));
    m.insert("order".into(), Value::Float(f.order));
    m.insert("color".into(), opt_str(&f.color));
    settings_fields("defaults", &f.defaults, &mut m);
    m
}

fn connection_fields(c: &Connection) -> Fields {
    let mut m = Fields::new();
    m.insert("name".into(), Value::Str(c.name.clone()));
    m.insert("folder".into(), opt_str(&c.folder));
    m.insert("order".into(), Value::Float(c.order));
    m.insert("kind".into(), Value::Str(kind_name(c.kind).into()));
    m.insert("endpoint".into(), json(&c.endpoint));
    m.insert("notes".into(), Value::Str(c.notes.clone()));
    m.insert("tags".into(), json(&c.tags));
    settings_fields("overrides", &c.overrides, &mut m);
    m
}

fn highlight_fields(h: &HighlightProfile) -> Fields {
    let mut m = Fields::new();
    m.insert("name".into(), Value::Str(h.name.clone()));
    m.insert("match_case".into(), Value::Bool(h.match_case));
    m.insert("rules".into(), json(&h.rules));
    m
}

fn kind_name(k: ConnectionKind) -> &'static str {
    match k {
        ConnectionKind::Ssh => "ssh",
        ConnectionKind::Telnet => "telnet",
        ConnectionKind::Serial => "serial",
        ConnectionKind::Rdp => "rdp",
        ConnectionKind::Vnc => "vnc",
    }
}

/// Every object in the tree as (kind, fields), keyed by id.
fn snapshot(t: &Tree) -> BTreeMap<String, (&'static str, Fields)> {
    let mut out = BTreeMap::new();
    let mut global = Fields::new();
    settings_fields("defaults", &t.global, &mut global);
    out.insert(GLOBAL_ID.to_string(), (KIND_SETTINGS, global));
    for f in t.folders() {
        out.insert(f.id.clone(), (KIND_FOLDER, folder_fields(f)));
    }
    for c in t.connections() {
        out.insert(c.id.clone(), (KIND_CONNECTION, connection_fields(c)));
    }
    for h in t.highlight_profiles.values() {
        out.insert(h.id.clone(), (KIND_HIGHLIGHT, highlight_fields(h)));
    }
    out
}

// ---- replica -> tree -----------------------------------------------------------

fn get_str(o: &ObjectState, field: &str) -> Option<String> {
    match o.get(field) {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

fn get_f64(o: &ObjectState, field: &str) -> f64 {
    match o.get(field) {
        Some(Value::Float(x)) => *x,
        Some(Value::Int(i)) => *i as f64,
        _ => 0.0,
    }
}

fn get_json<T: serde::de::DeserializeOwned>(o: &ObjectState, field: &str) -> Option<T> {
    serde_json::from_str(&get_str(o, field)?).ok()
}

fn get_settings(o: &ObjectState, prefix: &str) -> Settings {
    let mut obj = serde_json::Map::new();
    for name in Settings::FIELDS {
        if let Some(v) = get_str(o, &format!("{prefix}.{name}")).and_then(|s| serde_json::from_str(&s).ok()) {
            obj.insert(name.to_string(), v);
        }
    }
    serde_json::from_value(serde_json::Value::Object(obj)).unwrap_or_default()
}

/// Rebuild the tree from live objects. Data from other devices may be
/// incomplete or contradictory, so this never fails: a folder whose parent
/// is missing (or that sits in a cycle) goes to the top level, and so does a
/// connection whose folder is gone. Objects inside a deleted folder stay
/// hidden with it, and come back if the folder is restored.
pub fn project(replica: &Replica) -> Tree {
    let live: Vec<(&String, &ObjectState)> = replica.live_objects().collect();
    let global = live
        .iter()
        .find(|(id, o)| id.as_str() == GLOBAL_ID && o.kind == KIND_SETTINGS)
        .map(|(_, o)| get_settings(o, "defaults"))
        .unwrap_or_default();
    let mut tree = Tree::new(global);
    tree.highlight_profiles = live
        .iter()
        .filter(|(_, o)| o.kind == KIND_HIGHLIGHT)
        .map(|(id, o)| {
            let profile = HighlightProfile {
                id: id.to_string(),
                name: get_str(o, "name").unwrap_or_default(),
                match_case: matches!(o.get("match_case"), Some(Value::Bool(true))),
                rules: get_json(o, "rules").unwrap_or_default(),
            };
            (id.to_string(), profile)
        })
        .collect();

    let is_deleted_folder = |id: &str| {
        replica
            .object(id)
            .is_some_and(|o| o.kind == KIND_FOLDER && o.is_deleted())
    };

    let mut pending: Vec<Folder> = live
        .iter()
        .filter(|(_, o)| o.kind == KIND_FOLDER)
        .map(|(id, o)| Folder {
            id: id.to_string(),
            name: get_str(o, "name").unwrap_or_default(),
            parent: get_str(o, "parent"),
            order: get_f64(o, "order"),
            color: get_str(o, "color"),
            defaults: get_settings(o, "defaults"),
        })
        .filter(|f| !f.parent.as_deref().is_some_and(is_deleted_folder))
        .collect();
    // Parents before children; anything left over is orphaned or cyclic.
    loop {
        let before = pending.len();
        pending.retain(|f| {
            let ready = f.parent.as_deref().is_none_or(|p| tree.folder(p).is_ok());
            if ready {
                let _ = tree.add_folder(f.clone());
            }
            !ready
        });
        if pending.is_empty() || pending.len() == before {
            break;
        }
    }
    // A folder under a deleted ancestor stays hidden; true orphans surface at the top.
    let hidden: Vec<String> = pending
        .iter()
        .filter(|f| hidden_by_delete(replica, f.parent.as_deref()))
        .map(|f| f.id.clone())
        .collect();
    for mut f in pending.into_iter().filter(|f| !hidden.contains(&f.id)) {
        f.parent = None;
        let _ = tree.add_folder(f);
    }

    for (id, o) in live.iter().filter(|(_, o)| o.kind == KIND_CONNECTION) {
        let folder = get_str(o, "folder");
        if folder.as_deref().is_some_and(|f| hidden_by_delete(replica, Some(f))) {
            continue;
        }
        let kind = get_str(o, "kind")
            .and_then(|k| ConnectionKind::parse(&k))
            .unwrap_or(ConnectionKind::Ssh);
        let conn = Connection {
            id: id.to_string(),
            name: get_str(o, "name").unwrap_or_default(),
            folder: folder.filter(|f| tree.folder(f).is_ok()),
            order: get_f64(o, "order"),
            kind,
            endpoint: get_json(o, "endpoint").unwrap_or_else(|| Endpoint::network(kind, "")),
            overrides: get_settings(o, "overrides"),
            notes: get_str(o, "notes").unwrap_or_default(),
            tags: get_json(o, "tags").unwrap_or_default(),
        };
        let _ = tree.add_connection(conn);
    }
    tree
}

/// True if `folder` or one of its ancestors is a deleted folder.
fn hidden_by_delete<'a>(replica: &'a Replica, mut folder: Option<&'a str>) -> bool {
    let mut steps = 0;
    while let Some(id) = folder {
        let Some(o) = replica.object(id) else { return false };
        if o.kind != KIND_FOLDER {
            return false;
        }
        if o.is_deleted() {
            return true;
        }
        folder = match o.get("parent") {
            Some(Value::Str(p)) => Some(p.as_str()),
            _ => None,
        };
        steps += 1;
        if steps > 256 {
            return false; // cycle
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use linecard_core_model::Source;

    fn folder(id: &str, parent: Option<&str>) -> Folder {
        Folder {
            id: id.into(),
            name: id.to_uppercase(),
            parent: parent.map(Into::into),
            order: 0.0,
            color: None,
            defaults: Settings::default(),
        }
    }

    fn conn(id: &str, folder: Option<&str>) -> Connection {
        Connection {
            id: id.into(),
            name: id.into(),
            folder: folder.map(Into::into),
            order: 1.5,
            kind: ConnectionKind::Ssh,
            endpoint: Endpoint::Network {
                host: format!("{id}.lab"),
                port: 2222,
            },
            overrides: Settings {
                tab_color: Some("#d64545".into()),
                ..Default::default()
            },
            notes: "core switch".into(),
            tags: vec!["prod".into()],
        }
    }

    fn sample(t: &mut Tree) {
        t.global.keepalive_secs = Some(30);
        let mut dc = folder("dc", None);
        dc.defaults.username = Some("netadmin".into());
        t.add_folder(dc).unwrap();
        t.add_folder(folder("spine", Some("dc"))).unwrap();
        t.add_connection(conn("spine01", Some("spine"))).unwrap();
    }

    fn temp_db() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "linecard-lib-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("library.db")
    }

    #[test]
    fn survives_a_restart() {
        let path = temp_db();
        let device = {
            let lib = Library::open(&path, sample).unwrap();
            lib.device().to_string()
        };
        // Reopening neither re-seeds nor changes the device id.
        let lib = Library::open(&path, |_| panic!("seeded twice")).unwrap();
        assert_eq!(lib.device(), device);
        let t = lib.tree();
        let c = t.connection("spine01").unwrap();
        assert_eq!(c, &conn("spine01", Some("spine")));
        assert_eq!(t.path(Some("spine")).unwrap(), "/DC/SPINE");
        let r = t.resolve("spine01").unwrap();
        assert_eq!(r.settings.username.as_deref(), Some("netadmin"));
        assert_eq!(r.source_of("username"), Some(&Source::Folder("dc".into())));
        assert_eq!(r.settings.keepalive_secs, Some(30));
        assert!(lib.changes().is_empty(), "own history is not highlighted");
    }

    #[test]
    fn edits_become_minimal_ops() {
        let mut lib = Library::open_in_memory(sample).unwrap();
        let ops_before = lib.replica.log().len();
        lib.edit(|t| {
            t.connection_mut("spine01").map_err(err)?.name = "spine-01".into();
            Ok(())
        })
        .unwrap();
        let log = lib.replica.log();
        assert_eq!(log.len(), ops_before + 1, "one op for one changed object");
        assert_eq!(log.last().unwrap().changes.keys().collect::<Vec<_>>(), vec!["name"]);
        // A no-op edit writes nothing.
        lib.edit(|_| Ok(())).unwrap();
        assert_eq!(lib.replica.log().len(), ops_before + 1);
    }

    #[test]
    fn failed_edits_change_nothing() {
        let mut lib = Library::open_in_memory(sample).unwrap();
        let n = lib.replica.log().len();
        let r: Result<()> = lib.edit(|t| t.move_folder("dc", Some("spine")).map_err(err));
        assert!(r.is_err(), "cycle rejected");
        assert_eq!(lib.replica.log().len(), n);
        assert_eq!(lib.tree().path(Some("spine")).unwrap(), "/DC/SPINE");
    }

    #[test]
    fn deleting_a_folder_hides_its_contents_and_revert_restores_them() {
        let path = temp_db();
        let mut lib = Library::open(&path, sample).unwrap();
        lib.edit(|t| t.remove_folder("dc").map(|_| ()).map_err(err)).unwrap();
        assert_eq!(lib.tree().folders().count(), 0);
        assert_eq!(lib.tree().connections().count(), 0);

        // Undo the folder's delete op: the folder returns. Its contents were
        // deleted by their own ops and come back as each is reverted.
        let deletes: Vec<Hlc> = lib
            .replica
            .log()
            .iter()
            .filter(|o| o.action == linecard_core_sync::Action::Delete)
            .map(|o| o.id.clone())
            .collect();
        for op in &deletes {
            lib.revert(op).unwrap();
        }
        let lib = Library::open(&path, |_| {}).unwrap();
        assert_eq!(
            lib.tree().connection("spine01").unwrap().folder.as_deref(),
            Some("spine")
        );
        assert_eq!(lib.tree().folders().count(), 2);
    }

    #[test]
    fn two_libraries_converge_and_remote_changes_are_highlighted() {
        let mut a = Library::open_in_memory(sample).unwrap();
        let mut b = Library::open_in_memory(|_| {}).unwrap();
        // Only the app-wide settings object (same id on every device).
        assert!(b.own_ops_after(None).iter().all(|o| o.object == GLOBAL_ID));

        // A's whole history arrives at B.
        let n = b.apply_remote(a.own_ops_after(None)).unwrap();
        assert!(n > 0);
        assert_eq!(
            b.tree().connection("spine01").unwrap(),
            a.tree().connection("spine01").unwrap()
        );
        assert_eq!(b.apply_remote(a.own_ops_after(None)).unwrap(), 0, "duplicates ignored");
        assert!(b.changes().get("spine01").is_some(), "highlighted as added");

        // B edits; A gets just that.
        b.changes_mut().ack_all();
        let before = b.last_own_op();
        b.edit(|t| {
            t.connection_mut("spine01").map_err(err)?.name = "spine-1".into();
            Ok(())
        })
        .unwrap();
        let new = b.own_ops_after(before.as_ref());
        assert_eq!(new.len(), 1);
        a.apply_remote(new).unwrap();
        assert_eq!(a.tree().connection("spine01").unwrap().name, "spine-1");
        assert_eq!(
            a.changes().get("spine01").unwrap().kind,
            linecard_core_sync::ChangeKind::Edited
        );
    }

    #[test]
    fn seeding_is_remembered() {
        let a = Library::open_in_memory(sample).unwrap();
        assert_eq!(a.seeded_until(), a.last_own_op());
        assert!(a.seeded_until().is_some());
    }

    #[test]
    fn joining_drops_only_an_untouched_starter() {
        let mut fresh = Library::open_in_memory(sample).unwrap();
        let until = fresh.drop_untouched_seed().unwrap().expect("untouched");
        assert_eq!(fresh.tree().folders().count() + fresh.tree().connections().count(), 0);
        assert!(fresh.own_ops_after(Some(&until)).is_empty(), "nothing left to push");

        let mut used = Library::open_in_memory(sample).unwrap();
        used.edit(|t| {
            let id = t.connections().next().unwrap().id.clone();
            t.connection_mut(&id).unwrap().name = "mine".into();
            Ok(())
        })
        .unwrap();
        let before = used.tree().connections().count();
        assert_eq!(used.drop_untouched_seed().unwrap(), None);
        assert_eq!(used.tree().connections().count(), before, "user content is kept");
    }

    #[test]
    fn projection_tolerates_orphans_and_cycles() {
        let mut r = Replica::new("A");
        let f = |pairs: &[(&str, Value)]| -> Fields { pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect() };
        // a <-> b cycle (possible after concurrent moves on two devices)
        r.create("a", KIND_FOLDER, f(&[("name", "A".into()), ("parent", "b".into())]), 1);
        r.create("b", KIND_FOLDER, f(&[("name", "B".into()), ("parent", "a".into())]), 2);
        // connection in a folder that never arrived
        r.create(
            "c",
            KIND_CONNECTION,
            f(&[
                ("name", "c".into()),
                ("folder", "ghost".into()),
                ("kind", "telnet".into()),
            ]),
            3,
        );
        let t = project(&r);
        assert_eq!(t.folders().count(), 2, "cyclic folders surface instead of vanishing");
        let c = t.connection("c").unwrap();
        assert_eq!(c.folder, None);
        assert_eq!(c.kind, ConnectionKind::Telnet);
    }
}
