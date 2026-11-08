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

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use linecard_core_model::{
    Connection, ConnectionKind, Credential, Endpoint, Folder, HighlightProfile, Settings, Snippet, SyncedSecret, Tree,
    VaultMeta,
};
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
const KIND_SNIPPET: &str = "snippet";
const KIND_CREDENTIAL: &str = "credential";
const KIND_VAULT: &str = "vault";
const KIND_SECRET: &str = "secret";
/// Object id of the synced vault's salt and check value.
const VAULT_ID: &str = "vault";

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
            t.snippets.clear();
            t.credentials.clear();
            Ok(())
        })?;
        Ok(self.last_own_op())
    }

    /// The fewest ops that rebuild the current state (for compacting a relay group).
    pub fn snapshot_ops(&self) -> Vec<Op> {
        self.replica.snapshot()
    }

    /// Every op from every device, oldest first (to seed a new sync group).
    pub fn all_ops(&self) -> Vec<Op> {
        let mut ops = self.replica.log().to_vec();
        ops.sort_by(|a, b| a.id.cmp(&b.id));
        ops
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
        // The cursor doesn't move if this fails, so they're pulled again.
        self.commit(&fresh)?;
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
        // settings object is implicit until first written). Only objects that
        // differ, or that the replica lacks, are turned into fields: doing the
        // whole tree took most of an edit's time in a big library.
        let live = |id: &str| self.replica.object(id).is_some_and(|o| !o.is_deleted());
        let candidates = changed_ids(&self.tree, &next, live);
        let before: BTreeMap<String, (&'static str, Fields)> = candidates
            .iter()
            .filter(|id| live(id))
            .filter_map(|id| Some((id.clone(), object_fields(&self.tree, id)?)))
            .collect();
        let after: BTreeMap<String, (&'static str, Fields)> = candidates
            .iter()
            .filter_map(|id| Some((id.clone(), object_fields(&next, id)?)))
            .collect();
        // Making each op also applies it to the replica, so a failure part way
        // (or when saving) must put the replica back to what's on disk.
        let mut ops = Vec::new();
        let made: Result<()> = (|| {
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
            Ok(())
        })();
        if let Err(e) = made {
            self.rollback();
            return Err(e);
        }
        self.commit(&ops)?;
        Ok(out)
    }

    /// Undo one op (local or remote) with a new op.
    pub fn revert(&mut self, op: &Hlc) -> Result<()> {
        let op = self.replica.revert(op, now_ms()).map_err(err)?;
        self.commit(&[op])
    }

    /// Save ops already applied to the replica. If saving fails, the replica
    /// goes back to what's on disk, so it never holds changes that would be
    /// lost on restart (and sent to other computers meanwhile).
    fn commit(&mut self, ops: &[Op]) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        if let Err(e) = self.store.append_ops(ops) {
            self.rollback();
            return Err(err(e));
        }
        self.tree = project(&self.replica);
        Ok(())
    }

    /// Rebuild the replica from the saved ops, keeping unacknowledged changes.
    /// If even that fails, the in-memory replica is left as it is.
    fn rollback(&mut self) {
        let device = self.replica.device().to_string();
        if let Ok(mut saved) = self.store.load_replica(&device, now_ms()) {
            saved.changes = std::mem::take(&mut self.replica.changes);
            self.replica = saved;
        }
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
    m.insert("forwards".into(), json(&c.forwards));
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

fn snippet_fields(s: &Snippet) -> Fields {
    let mut m = Fields::new();
    m.insert("name".into(), Value::Str(s.name.clone()));
    m.insert("text".into(), Value::Str(s.text.clone()));
    m.insert("folder".into(), opt_str(&s.folder));
    m.insert("order".into(), Value::Float(s.order));
    m.insert("enter".into(), Value::Bool(s.enter));
    m
}

fn credential_fields(c: &Credential) -> Fields {
    let mut m = Fields::new();
    m.insert("name".into(), Value::Str(c.name.clone()));
    m.insert("username".into(), Value::Str(c.username.clone()));
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

/// Ids an edit may have to write: objects that differ between `old` and
/// `new` (added, removed or changed), plus those the replica doesn't hold yet.
fn changed_ids(old: &Tree, new: &Tree, live: impl Fn(&str) -> bool) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut check = |id: &str, same: bool| {
        if !same || !live(id) {
            out.insert(id.to_string());
        }
    };
    check(GLOBAL_ID, old.global == new.global);
    if old.vault.is_some() || new.vault.is_some() {
        check(VAULT_ID, old.vault == new.vault);
    }
    for f in new.folders() {
        check(&f.id, old.folder(&f.id).ok() == Some(f));
    }
    for c in new.connections() {
        check(&c.id, old.connection(&c.id).ok() == Some(c));
    }
    for (id, h) in &new.highlight_profiles {
        check(id, old.highlight_profiles.get(id) == Some(h));
    }
    for (id, s) in &new.snippets {
        check(id, old.snippets.get(id) == Some(s));
    }
    for (id, s) in &new.secrets {
        check(id, old.secrets.get(id) == Some(s));
    }
    for (id, c) in &new.credentials {
        check(id, old.credentials.get(id) == Some(c));
    }
    // Removed ones.
    let gone = old
        .folders()
        .map(|f| &f.id)
        .filter(|id| new.folder(id).is_err())
        .chain(
            old.connections()
                .map(|c| &c.id)
                .filter(|id| new.connection(id).is_err()),
        )
        .chain(
            old.highlight_profiles
                .keys()
                .filter(|id| !new.highlight_profiles.contains_key(*id)),
        )
        .chain(old.snippets.keys().filter(|id| !new.snippets.contains_key(*id)))
        .chain(old.secrets.keys().filter(|id| !new.secrets.contains_key(*id)))
        .chain(old.credentials.keys().filter(|id| !new.credentials.contains_key(*id)));
    out.extend(gone.cloned());
    out
}

/// One object of the tree as (kind, fields), the same as [`snapshot`] gives
/// for its id (when ids collide across kinds, the same kind wins).
fn object_fields(t: &Tree, id: &str) -> Option<(&'static str, Fields)> {
    if let Some(s) = t.secrets.get(id) {
        let mut m = Fields::new();
        m.insert("key".into(), Value::Str(s.key.clone()));
        m.insert("label".into(), Value::Str(s.label.clone()));
        m.insert("blob".into(), Value::Str(s.blob.clone()));
        return Some((KIND_SECRET, m));
    }
    if id == VAULT_ID {
        if let Some(v) = &t.vault {
            let mut m = Fields::new();
            m.insert("salt".into(), Value::Str(v.salt.clone()));
            m.insert("check".into(), Value::Str(v.check.clone()));
            return Some((KIND_VAULT, m));
        }
    }
    if let Some(s) = t.snippets.get(id) {
        return Some((KIND_SNIPPET, snippet_fields(s)));
    }
    if let Some(c) = t.credentials.get(id) {
        return Some((KIND_CREDENTIAL, credential_fields(c)));
    }
    if let Some(h) = t.highlight_profiles.get(id) {
        return Some((KIND_HIGHLIGHT, highlight_fields(h)));
    }
    if let Ok(c) = t.connection(id) {
        return Some((KIND_CONNECTION, connection_fields(c)));
    }
    if let Ok(f) = t.folder(id) {
        return Some((KIND_FOLDER, folder_fields(f)));
    }
    if id == GLOBAL_ID {
        let mut global = Fields::new();
        settings_fields("defaults", &t.global, &mut global);
        return Some((KIND_SETTINGS, global));
    }
    None
}

/// Every object in the tree as (kind, fields), keyed by id.
#[cfg(test)]
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
    for s in t.snippets.values() {
        out.insert(s.id.clone(), (KIND_SNIPPET, snippet_fields(s)));
    }
    for c in t.credentials.values() {
        out.insert(c.id.clone(), (KIND_CREDENTIAL, credential_fields(c)));
    }
    if let Some(v) = &t.vault {
        let mut m = Fields::new();
        m.insert("salt".into(), Value::Str(v.salt.clone()));
        m.insert("check".into(), Value::Str(v.check.clone()));
        out.insert(VAULT_ID.to_string(), (KIND_VAULT, m));
    }
    for s in t.secrets.values() {
        let mut m = Fields::new();
        m.insert("key".into(), Value::Str(s.key.clone()));
        m.insert("label".into(), Value::Str(s.label.clone()));
        m.insert("blob".into(), Value::Str(s.blob.clone()));
        out.insert(s.id.clone(), (KIND_SECRET, m));
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
    tree.vault = live
        .iter()
        .find(|(id, o)| id.as_str() == VAULT_ID && o.kind == KIND_VAULT)
        .and_then(|(_, o)| {
            Some(VaultMeta {
                salt: get_str(o, "salt")?,
                check: get_str(o, "check")?,
            })
        });
    tree.secrets = live
        .iter()
        .filter(|(_, o)| o.kind == KIND_SECRET)
        .filter_map(|(id, o)| {
            Some((
                id.to_string(),
                SyncedSecret {
                    id: id.to_string(),
                    key: get_str(o, "key")?,
                    label: get_str(o, "label").unwrap_or_default(),
                    blob: get_str(o, "blob")?,
                },
            ))
        })
        .collect();
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

    tree.snippets = live
        .iter()
        .filter(|(_, o)| o.kind == KIND_SNIPPET)
        .map(|(id, o)| {
            let snippet = Snippet {
                id: id.to_string(),
                name: get_str(o, "name").unwrap_or_default(),
                text: get_str(o, "text").unwrap_or_default(),
                folder: get_str(o, "folder"),
                order: get_f64(o, "order"),
                enter: matches!(o.get("enter"), Some(Value::Bool(true))),
            };
            (id.to_string(), snippet)
        })
        .collect();

    tree.credentials = live
        .iter()
        .filter(|(_, o)| o.kind == KIND_CREDENTIAL)
        .map(|(id, o)| {
            let c = Credential {
                id: id.to_string(),
                name: get_str(o, "name").unwrap_or_default(),
                username: get_str(o, "username").unwrap_or_default(),
            };
            (id.to_string(), c)
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
            forwards: get_json(o, "forwards").unwrap_or_default(),
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
            forwards: vec![],
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

    /// Edits look only at objects that changed; what they record must still
    /// be everything that changed, every kind of object included.
    #[test]
    fn edits_record_exactly_what_changed() {
        let mut lib = Library::open_in_memory(sample).unwrap();
        type Step = Box<dyn Fn(&mut Tree)>;
        let steps: Vec<Step> = vec![
            Box::new(|t| t.global.keepalive_secs = Some(60)),
            Box::new(|t| t.connection_mut("spine01").unwrap().notes = "moved racks".into()),
            Box::new(|t| t.add_connection(conn("leaf01", Some("dc"))).unwrap()),
            Box::new(|t| {
                t.remove_connection("spine01").unwrap();
                t.add_folder(folder("edge", None)).unwrap();
            }),
            Box::new(|t| {
                t.snippets.insert(
                    "s1".into(),
                    Snippet {
                        id: "s1".into(),
                        name: "Show run".into(),
                        text: "show run".into(),
                        folder: None,
                        order: 1.0,
                        enter: true,
                    },
                );
                t.highlight_profiles.insert(
                    "h1".into(),
                    HighlightProfile {
                        id: "h1".into(),
                        name: "Errors".into(),
                        ..Default::default()
                    },
                );
            }),
            Box::new(|t| {
                t.vault = Some(VaultMeta {
                    salt: "s".into(),
                    check: "c".into(),
                });
                t.secrets.insert(
                    "p1".into(),
                    SyncedSecret {
                        id: "p1".into(),
                        key: "k".into(),
                        label: "admin".into(),
                        blob: "b".into(),
                    },
                );
            }),
            Box::new(|t| {
                t.snippets.clear();
                t.secrets.clear();
                t.vault = None;
                t.remove_folder("spine").unwrap();
            }),
        ];
        for (i, step) in steps.iter().enumerate() {
            let mut want = lib.tree().clone();
            step(&mut want);
            lib.edit(|t| {
                step(t);
                Ok(())
            })
            .unwrap();
            // The tree is rebuilt from the recorded ops, so a missed change shows here.
            assert_eq!(snapshot(lib.tree()), snapshot(&want), "step {i}");
        }
        let before = lib.replica.log().len();
        lib.edit(|_| Ok(())).unwrap();
        assert_eq!(
            lib.replica.log().len(),
            before,
            "an edit that changes nothing records nothing"
        );
    }

    #[test]
    fn a_save_that_fails_leaves_nothing_behind() {
        let path = temp_db();
        let mut lib = Library::open(&path, sample).unwrap();
        let ops_before = lib.replica.log().len();
        let pending_before = lib.own_ops_after(None).len();

        // Make every save fail, the way a full or locked disk would.
        let other = rusqlite::Connection::open(&path).unwrap();
        other
            .execute_batch("CREATE TRIGGER fail_saves BEFORE INSERT ON ops BEGIN SELECT RAISE(FAIL, 'disk full'); END;")
            .unwrap();
        let renamed = lib.edit(|t| {
            t.connection_mut("spine01").unwrap().name = "renamed".into();
            t.add_connection(conn("leaf01", Some("spine"))).map_err(err)
        });
        assert!(renamed.is_err());
        assert_eq!(lib.replica.log().len(), ops_before, "no unsaved ops left in memory");
        assert_eq!(lib.own_ops_after(None).len(), pending_before, "nothing unsaved to send");
        assert_eq!(lib.tree().connection("spine01").unwrap().name, "spine01");
        assert!(lib.tree().connection("leaf01").is_err());

        // Once saving works again, so does editing.
        other.execute_batch("DROP TRIGGER fail_saves;").unwrap();
        lib.edit(|t| t.add_connection(conn("leaf01", Some("spine"))).map_err(err))
            .unwrap();
        drop(lib);
        let lib = Library::open(&path, sample).unwrap();
        assert!(lib.tree().connection("leaf01").is_ok());
        assert_eq!(lib.tree().connection("spine01").unwrap().name, "spine01");
    }

    #[test]
    fn snippets_survive_a_restart_and_removal() {
        let path = temp_db();
        {
            let mut lib = Library::open(&path, sample).unwrap();
            lib.edit(|t| {
                t.snippets.insert(
                    "s1".into(),
                    Snippet {
                        id: "s1".into(),
                        name: "Show run".into(),
                        text: "show run
"
                        .into(),
                        folder: Some("dc".into()),
                        order: 2.0,
                        enter: true,
                    },
                );
                Ok(())
            })
            .unwrap();
        }
        let mut lib = Library::open(&path, |_| {}).unwrap();
        let s = &lib.tree().snippets["s1"];
        assert_eq!(
            (s.name.as_str(), s.text.as_str(), s.folder.as_deref()),
            (
                "Show run",
                "show run
",
                Some("dc")
            )
        );
        assert!(s.enter && s.order == 2.0);
        lib.edit(|t| {
            t.snippets.remove("s1");
            Ok(())
        })
        .unwrap();
        assert!(Library::open(&path, |_| {}).unwrap().tree().snippets.is_empty());
    }

    #[test]
    fn credentials_and_logon_actions_survive_a_restart() {
        use linecard_core_model::LogonStep;
        let path = temp_db();
        let steps = vec![
            LogonStep {
                expect: ">".into(),
                send: "enable".into(),
                password_of: None,
            },
            LogonStep {
                expect: "Password:".into(),
                send: String::new(),
                password_of: Some("c1".into()),
            },
        ];
        {
            let mut lib = Library::open(&path, sample).unwrap();
            lib.edit(|t| {
                t.credentials.insert(
                    "c1".into(),
                    Credential {
                        id: "c1".into(),
                        name: "NetOps".into(),
                        username: "admin".into(),
                    },
                );
                let f = t.folder_mut("dc").map_err(|e| e.to_string())?;
                f.defaults.credential_id = Some("c1".into());
                f.defaults.logon_actions = Some(steps.clone());
                Ok(())
            })
            .unwrap();
        }
        let mut lib = Library::open(&path, |_| {}).unwrap();
        assert_eq!(lib.tree().credentials["c1"].username, "admin");
        let dc = lib.tree().folder("dc").unwrap();
        assert_eq!(dc.defaults.credential_id.as_deref(), Some("c1"));
        assert_eq!(dc.defaults.logon_actions.as_ref(), Some(&steps));
        lib.edit(|t| {
            t.credentials.remove("c1");
            Ok(())
        })
        .unwrap();
        assert!(Library::open(&path, |_| {}).unwrap().tree().credentials.is_empty());
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
