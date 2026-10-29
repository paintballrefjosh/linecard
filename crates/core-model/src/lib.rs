//! Linecard's object model.
//!
//! Folders form a tree; connections live in folders. Most settings are
//! *inheritable*: a connection's effective value comes from its own override,
//! else the nearest folder that sets it, else the global defaults.
//! [`Tree::resolve`] computes the effective settings and records where each
//! value came from, so the UI can show "inherited from /Datacenter/Spine".
//!
//! This crate has no required dependencies so its logic is easy to test.

mod highlight;
mod kind;
mod settings;
mod tree;

pub use highlight::{HighlightProfile, HighlightRule};
pub use kind::{ConnectionKind, Endpoint, FlowControl, Parity, SerialSettings};
pub use settings::{Resolved, Settings, Source};
pub use tree::{Connection, Folder, Tree, TreeError};

/// Stable identifier for any synced object (UUIDv7 string in practice).
/// Generation lives in the app layer so this crate stays dependency-free.
pub type ObjectId = String;
