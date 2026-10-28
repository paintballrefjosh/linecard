use std::collections::BTreeMap;
use std::fmt;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::Hlc;

/// Reserved field holding an object's tombstone flag.
pub const DELETED_FIELD: &str = "_deleted";

/// A field value. Rich values (endpoints, settings blocks) are stored as JSON
/// text in `Str` by the app layer; the engine only needs equality and display.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(untagged))]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.to_string())
    }
}
impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Str(s)
    }
}
impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}
impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Int(i)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "(none)"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{x}"),
            Value::Str(s) => write!(f, "{s}"),
        }
    }
}

/// What kind of change an op represents. The merge treats them all as field
/// writes; the action drives how the change is shown to the user.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(tag = "action", rename_all = "lowercase")
)]
pub enum Action {
    Create,
    Update,
    Delete,
    Restore,
    /// Undo of another op (which may itself be a revert).
    Revert {
        of: Hlc,
    },
}

/// One change to one object.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Op {
    /// Unique id and merge timestamp.
    pub id: Hlc,
    pub object: String,
    /// "folder", "connection", "credential", "device", ...
    pub object_kind: String,
    pub action: Action,
    /// Fields written by this op.
    pub changes: BTreeMap<String, Value>,
    /// Values those fields had on the authoring device just before the op.
    /// `None` = the field did not exist.
    pub prev: BTreeMap<String, Option<Value>>,
}

impl Op {
    pub fn device(&self) -> &str {
        &self.id.device
    }
}
