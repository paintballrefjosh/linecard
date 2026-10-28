use std::collections::BTreeMap;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{Action, Hlc, Op, Value};

/// How a remotely changed object is highlighted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(rename_all = "lowercase"))]
pub enum ChangeKind {
    /// Green.
    Added,
    /// Amber.
    Edited,
    /// Amber, location changed only.
    Moved,
    /// Red with strikethrough.
    Deleted,
    /// Green: a deleted object came back.
    Restored,
    /// Amber: someone reverted a change.
    Reverted,
}

/// Fields whose change alone means "moved", not "edited".
const PLACEMENT_FIELDS: &[&str] = &["folder", "parent", "order"];

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct FieldDiff {
    pub field: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
}

/// One remote op's contribution to a pending change (shown as a line in the
/// hover card; the Revert button reverts `op`).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ChangeEntry {
    pub op: Hlc,
    pub kind: ChangeKind,
    pub fields: Vec<FieldDiff>,
}

/// Everything not yet acknowledged about one object.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct PendingChange {
    pub object: String,
    pub object_kind: String,
    /// Overall highlight for the object.
    pub kind: ChangeKind,
    pub entries: Vec<ChangeEntry>,
}

impl PendingChange {
    /// Devices that contributed, for "edited on Desktop-Home".
    pub fn devices(&self) -> Vec<&str> {
        let mut d: Vec<&str> = self.entries.iter().map(|e| e.op.device.as_str()).collect();
        d.dedup();
        d
    }
}

/// Unacknowledged remote changes on this device. Not synced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChangeTracker {
    pending: BTreeMap<String, PendingChange>,
}

impl ChangeTracker {
    pub fn record(&mut self, op: &Op, fields: Vec<FieldDiff>) {
        let kind = classify(op, &fields);
        let entry = ChangeEntry {
            op: op.id.clone(),
            kind,
            fields,
        };
        match self.pending.get_mut(&op.object) {
            None => {
                self.pending.insert(
                    op.object.clone(),
                    PendingChange {
                        object: op.object.clone(),
                        object_kind: op.object_kind.clone(),
                        kind,
                        entries: vec![entry],
                    },
                );
            }
            Some(p) => {
                // Added then deleted before the user ever looked: nothing to show.
                if p.kind == ChangeKind::Added && kind == ChangeKind::Deleted {
                    self.pending.remove(&op.object);
                    return;
                }
                p.kind = combine(p.kind, kind);
                p.entries.push(entry);
            }
        }
    }

    pub fn get(&self, object: &str) -> Option<&PendingChange> {
        self.pending.get(object)
    }

    pub fn iter(&self) -> impl Iterator<Item = &PendingChange> {
        self.pending.values()
    }

    pub fn ack(&mut self, object: &str) {
        self.pending.remove(object);
    }

    pub fn ack_all(&mut self) {
        self.pending.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Counts per kind for the change bar ("3 added · 2 edited · 1 deleted").
    pub fn counts(&self) -> BTreeMap<ChangeKind, usize> {
        let mut m = BTreeMap::new();
        for p in self.pending.values() {
            *m.entry(p.kind).or_insert(0) += 1;
        }
        m
    }
}

fn classify(op: &Op, fields: &[FieldDiff]) -> ChangeKind {
    match op.action {
        Action::Create => ChangeKind::Added,
        Action::Delete => ChangeKind::Deleted,
        Action::Restore => ChangeKind::Restored,
        Action::Revert { .. } => ChangeKind::Reverted,
        Action::Update => {
            if !fields.is_empty() && fields.iter().all(|f| PLACEMENT_FIELDS.contains(&f.field.as_str())) {
                ChangeKind::Moved
            } else {
                ChangeKind::Edited
            }
        }
    }
}

fn combine(existing: ChangeKind, new: ChangeKind) -> ChangeKind {
    use ChangeKind::*;
    match (existing, new) {
        (_, Deleted) => Deleted,
        // Still new to this device, whatever happened to it since.
        (Added, _) => Added,
        (Edited, Moved) => Edited,
        (_, k) => k,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(action: Action, object: &str, n: u32) -> Op {
        Op {
            id: Hlc {
                wall_ms: 1,
                counter: n,
                device: "remote".into(),
            },
            object: object.into(),
            object_kind: "connection".into(),
            action,
            changes: BTreeMap::new(),
            prev: BTreeMap::new(),
        }
    }

    fn diff(field: &str) -> Vec<FieldDiff> {
        vec![FieldDiff {
            field: field.into(),
            before: None,
            after: Some(Value::from("x")),
        }]
    }

    #[test]
    fn classification_and_combination() {
        let mut t = ChangeTracker::default();
        t.record(&op(Action::Update, "a", 1), diff("folder"));
        assert_eq!(t.get("a").unwrap().kind, ChangeKind::Moved);
        t.record(&op(Action::Update, "a", 2), diff("host"));
        assert_eq!(t.get("a").unwrap().kind, ChangeKind::Edited);
        t.record(&op(Action::Update, "a", 3), diff("order"));
        assert_eq!(t.get("a").unwrap().kind, ChangeKind::Edited);
        assert_eq!(t.get("a").unwrap().entries.len(), 3);

        t.record(&op(Action::Create, "b", 4), diff("host"));
        t.record(&op(Action::Update, "b", 5), diff("host"));
        assert_eq!(t.get("b").unwrap().kind, ChangeKind::Added);
        t.record(&op(Action::Delete, "b", 6), diff("_deleted"));
        assert!(t.get("b").is_none(), "added+deleted collapses away");

        t.record(&op(Action::Delete, "a", 7), diff("_deleted"));
        assert_eq!(t.get("a").unwrap().kind, ChangeKind::Deleted);
        assert_eq!(t.counts().get(&ChangeKind::Deleted), Some(&1));
    }
}
