use std::collections::{BTreeMap, HashSet};
use std::fmt;

use crate::changes::{ChangeTracker, FieldDiff};
use crate::{Action, Clock, DeviceId, Hlc, Op, Value, DELETED_FIELD};

/// Current state of one object: each field with the timestamp of the op that
/// last won it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ObjectState {
    pub kind: String,
    pub fields: BTreeMap<String, (Value, Hlc)>,
}

impl ObjectState {
    pub fn get(&self, field: &str) -> Option<&Value> {
        self.fields.get(field).map(|(v, _)| v)
    }

    pub fn is_deleted(&self) -> bool {
        matches!(self.get(DELETED_FIELD), Some(Value::Bool(true)) | None)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SyncError {
    UnknownOp(Hlc),
    UnknownObject(String),
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncError::UnknownOp(id) => write!(f, "op {id} not in the log"),
            SyncError::UnknownObject(id) => write!(f, "object {id} not found"),
        }
    }
}

impl std::error::Error for SyncError {}

/// One device's copy of the synced data.
#[derive(Debug, Clone)]
pub struct Replica {
    clock: Clock,
    objects: BTreeMap<String, ObjectState>,
    log: Vec<Op>,
    seen: HashSet<Hlc>,
    /// Remote changes awaiting acknowledgement on this device.
    pub changes: ChangeTracker,
}

impl Replica {
    pub fn new(device: impl Into<DeviceId>) -> Self {
        Replica {
            clock: Clock::new(device),
            objects: BTreeMap::new(),
            log: Vec::new(),
            seen: HashSet::new(),
            changes: ChangeTracker::default(),
        }
    }

    pub fn device(&self) -> &DeviceId {
        self.clock.device()
    }

    pub fn object(&self, id: &str) -> Option<&ObjectState> {
        self.objects.get(id)
    }

    /// Live (non-deleted) objects.
    pub fn live_objects(&self) -> impl Iterator<Item = (&String, &ObjectState)> {
        self.objects.iter().filter(|(_, o)| !o.is_deleted())
    }

    /// Every object including tombstones (the UI shows unacknowledged deletes).
    pub fn all_objects(&self) -> impl Iterator<Item = (&String, &ObjectState)> {
        self.objects.iter()
    }

    pub fn log(&self) -> &[Op] {
        &self.log
    }

    pub fn op(&self, id: &Hlc) -> Option<&Op> {
        self.log.iter().find(|o| &o.id == id)
    }

    /// All ops that touched `object`, oldest first (per-object history).
    pub fn history(&self, object: &str) -> Vec<&Op> {
        let mut v: Vec<_> = self.log.iter().filter(|o| o.object == object).collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// Ops this replica has that `since` hasn't (by id), for pushing.
    pub fn ops_after(&self, since: Option<&Hlc>) -> Vec<&Op> {
        self.log.iter().filter(|o| since.is_none_or(|s| &o.id > s)).collect()
    }

    // ---- local edits -------------------------------------------------

    /// Create an object. Returns the op to send to other devices.
    pub fn create(&mut self, object: &str, kind: &str, fields: BTreeMap<String, Value>, now_ms: u64) -> Op {
        let mut changes = fields;
        changes.insert(DELETED_FIELD.into(), Value::Bool(false));
        self.local(object, kind, Action::Create, changes, now_ms)
    }

    pub fn update(&mut self, object: &str, fields: BTreeMap<String, Value>, now_ms: u64) -> Result<Op, SyncError> {
        let kind = self.kind_of(object)?;
        Ok(self.local(object, &kind, Action::Update, fields, now_ms))
    }

    pub fn delete(&mut self, object: &str, now_ms: u64) -> Result<Op, SyncError> {
        let kind = self.kind_of(object)?;
        let changes = BTreeMap::from([(DELETED_FIELD.to_string(), Value::Bool(true))]);
        Ok(self.local(object, &kind, Action::Delete, changes, now_ms))
    }

    pub fn restore(&mut self, object: &str, now_ms: u64) -> Result<Op, SyncError> {
        let kind = self.kind_of(object)?;
        let changes = BTreeMap::from([(DELETED_FIELD.to_string(), Value::Bool(false))]);
        Ok(self.local(object, &kind, Action::Restore, changes, now_ms))
    }

    /// Undo `target` by writing back the values it replaced. The revert is an
    /// ordinary op, so it syncs everywhere and can itself be reverted.
    pub fn revert(&mut self, target: &Hlc, now_ms: u64) -> Result<Op, SyncError> {
        let t = self
            .op(target)
            .ok_or_else(|| SyncError::UnknownOp(target.clone()))?
            .clone();
        let changes = t
            .prev
            .iter()
            .map(|(field, prev)| {
                let v = match prev {
                    Some(v) => v.clone(),
                    // The object didn't exist before: reverting means deleting it.
                    None if field == DELETED_FIELD => Value::Bool(true),
                    None => Value::Null,
                };
                (field.clone(), v)
            })
            .collect();
        Ok(self.local(
            &t.object,
            &t.object_kind,
            Action::Revert { of: t.id.clone() },
            changes,
            now_ms,
        ))
    }

    fn kind_of(&self, object: &str) -> Result<String, SyncError> {
        self.objects
            .get(object)
            .map(|o| o.kind.clone())
            .ok_or_else(|| SyncError::UnknownObject(object.to_string()))
    }

    fn local(&mut self, object: &str, kind: &str, action: Action, changes: BTreeMap<String, Value>, now_ms: u64) -> Op {
        let id = self.clock.tick(now_ms);
        let current = self.objects.get(object);
        let prev = changes
            .keys()
            .map(|f| (f.clone(), current.and_then(|o| o.get(f).cloned())))
            .collect();
        let op = Op {
            id,
            object: object.into(),
            object_kind: kind.into(),
            action,
            changes,
            prev,
        };
        self.integrate(&op);
        // Our own edits aren't highlighted; a newer local edit also supersedes
        // anything pending for that object.
        self.changes.ack(object);
        op
    }

    // ---- remote ops -----------------------------------------------------

    /// Apply an op from another device. Returns false if it was already seen.
    pub fn apply_remote(&mut self, op: Op, now_ms: u64) -> bool {
        if self.seen.contains(&op.id) {
            return false;
        }
        self.clock.observe(&op.id, now_ms);
        let diffs = self.integrate(&op);
        if op.device() != self.device() && !diffs.is_empty() {
            self.changes.record(&op, diffs);
        }
        true
    }

    /// Merge one op: each field is taken only if this op is newer than the op
    /// that last wrote it. Returns the fields whose visible value changed.
    fn integrate(&mut self, op: &Op) -> Vec<FieldDiff> {
        self.seen.insert(op.id.clone());
        self.log.push(op.clone());
        let obj = self.objects.entry(op.object.clone()).or_insert_with(|| ObjectState {
            kind: op.object_kind.clone(),
            fields: BTreeMap::new(),
        });
        let mut diffs = Vec::new();
        for (field, value) in &op.changes {
            let wins = obj.fields.get(field).is_none_or(|(_, stamp)| &op.id > stamp);
            if !wins {
                continue;
            }
            let before = obj.get(field).cloned();
            obj.fields.insert(field.clone(), (value.clone(), op.id.clone()));
            if before.as_ref() != Some(value) {
                diffs.push(FieldDiff {
                    field: field.clone(),
                    before,
                    after: Some(value.clone()),
                });
            }
        }
        diffs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChangeKind;

    fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), Value::from(*v))).collect()
    }

    fn visible(r: &Replica) -> Vec<(String, Vec<(String, String)>)> {
        r.all_objects()
            .map(|(id, o)| {
                (
                    id.clone(),
                    o.fields.iter().map(|(k, (v, _))| (k.clone(), v.to_string())).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn concurrent_edits_converge_in_any_order() {
        let mut a = Replica::new("A");
        let mut b = Replica::new("B");
        let create = a.create(
            "c1",
            "connection",
            fields(&[("host", "10.0.0.1"), ("name", "sw1")]),
            100,
        );
        b.apply_remote(create.clone(), 100);

        // Concurrent: A edits host, B edits host and name.
        let ea = a.update("c1", fields(&[("host", "10.0.0.2")]), 200).unwrap();
        let eb = b
            .update("c1", fields(&[("host", "10.0.0.3"), ("name", "sw1-new")]), 200)
            .unwrap();

        a.apply_remote(eb.clone(), 201);
        b.apply_remote(ea.clone(), 201);

        // A third replica that receives everything in a different order.
        let mut c = Replica::new("C");
        for op in [eb, ea, create] {
            c.apply_remote(op, 300);
        }

        assert_eq!(visible(&a), visible(&b));
        assert_eq!(visible(&a), visible(&c));
        // Same wall time: device "B" > "A" breaks the tie.
        assert_eq!(a.object("c1").unwrap().get("host"), Some(&Value::from("10.0.0.3")));
        assert_eq!(a.object("c1").unwrap().get("name"), Some(&Value::from("sw1-new")));
    }

    #[test]
    fn delete_keeps_concurrent_edit_for_restore() {
        let mut a = Replica::new("A");
        let mut b = Replica::new("B");
        let create = a.create("c1", "connection", fields(&[("host", "old")]), 100);
        b.apply_remote(create, 100);

        let del = a.delete("c1", 200).unwrap();
        let edit = b.update("c1", fields(&[("host", "new")]), 210).unwrap();
        a.apply_remote(edit, 211);
        b.apply_remote(del.clone(), 211);

        assert!(a.object("c1").unwrap().is_deleted());
        assert!(b.object("c1").unwrap().is_deleted());

        let undo = a.revert(&del.id, 300).unwrap();
        b.apply_remote(undo, 301);
        for r in [&a, &b] {
            let o = r.object("c1").unwrap();
            assert!(!o.is_deleted());
            assert_eq!(o.get("host"), Some(&Value::from("new")), "the edit survived the delete");
        }
    }

    #[test]
    fn revert_of_revert_reapplies() {
        let mut a = Replica::new("A");
        a.create("c1", "connection", fields(&[("host", "1")]), 100);
        let edit = a.update("c1", fields(&[("host", "2")]), 200).unwrap();
        let r1 = a.revert(&edit.id, 300).unwrap();
        assert_eq!(a.object("c1").unwrap().get("host"), Some(&Value::from("1")));
        a.revert(&r1.id, 400).unwrap();
        assert_eq!(a.object("c1").unwrap().get("host"), Some(&Value::from("2")));
        assert_eq!(a.history("c1").len(), 4);
    }

    #[test]
    fn revert_of_create_deletes() {
        let mut a = Replica::new("A");
        let c = a.create("c1", "connection", fields(&[("host", "1")]), 100);
        a.revert(&c.id, 200).unwrap();
        assert!(a.object("c1").unwrap().is_deleted());
    }

    #[test]
    fn duplicate_ops_are_ignored() {
        let mut a = Replica::new("A");
        let mut b = Replica::new("B");
        let op = a.create("c1", "connection", fields(&[("host", "1")]), 100);
        assert!(b.apply_remote(op.clone(), 100));
        assert!(!b.apply_remote(op, 100));
        assert_eq!(b.log().len(), 1);
    }

    #[test]
    fn remote_changes_are_tracked_and_acked() {
        let mut a = Replica::new("A");
        let mut b = Replica::new("B");
        let create = a.create("c1", "connection", fields(&[("host", "1")]), 100);
        b.apply_remote(create, 100);
        assert_eq!(b.changes.get("c1").unwrap().kind, ChangeKind::Added);

        b.changes.ack("c1");
        let edit = a.update("c1", fields(&[("host", "2")]), 200).unwrap();
        b.apply_remote(edit, 200);
        let pending = b.changes.get("c1").unwrap();
        assert_eq!(pending.kind, ChangeKind::Edited);
        let diff = &pending.entries[0].fields[0];
        assert_eq!(diff.before, Some(Value::from("1")));
        assert_eq!(diff.after, Some(Value::from("2")));

        // A local edit on B clears B's highlight for that object.
        b.update("c1", fields(&[("host", "3")]), 300).unwrap();
        assert!(b.changes.get("c1").is_none());
        // The authoring device never highlights its own changes.
        assert!(a.changes.is_empty());
    }

    #[test]
    fn losing_concurrent_write_is_not_highlighted() {
        let mut a = Replica::new("A");
        let mut b = Replica::new("B");
        let create = a.create("c1", "connection", fields(&[("host", "1")]), 100);
        b.apply_remote(create, 100);
        b.changes.ack_all();
        let newer = b.update("c1", fields(&[("host", "b")]), 500).unwrap();
        let older = a.update("c1", fields(&[("host", "a")]), 400).unwrap();
        b.apply_remote(older, 501); // loses LWW on B: nothing visible changed
        assert!(b.changes.is_empty());
        a.apply_remote(newer, 501);
        assert_eq!(a.object("c1").unwrap().get("host"), Some(&Value::from("b")));
    }
}
