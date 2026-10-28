//! Linecard's sync engine core.
//!
//! * [`Hlc`] / [`Clock`] — hybrid logical clocks give every operation a
//!   globally ordered, unique timestamp even when device clocks drift.
//! * [`Op`] — one change to one object. Ops carry the previous values of the
//!   fields they touch, which powers "what changed" and one-click revert.
//! * [`Replica`] — applies local and remote ops with per-field
//!   last-writer-wins, so every device converges to the same state no matter
//!   the order ops arrive in. Deletes are a `_deleted` field (a tombstone),
//!   so a delete can always be reverted and concurrent edits are not lost.
//! * [`ChangeTracker`] — remote changes waiting for the user to acknowledge
//!   them (green added, amber edited, red deleted).
//!
//! Transports and encryption live in other crates; this one is pure logic
//! with no required dependencies.

mod changes;
mod clock;
mod op;
mod replica;

pub use changes::{ChangeEntry, ChangeKind, ChangeTracker, FieldDiff, PendingChange};
pub use clock::{Clock, DeviceId, Hlc};
pub use op::{Action, Op, Value, DELETED_FIELD};
pub use replica::{ObjectState, Replica, SyncError};

/// Version of the op format. Bump when the wire format changes in a way old
/// clients can't preserve; shown on the Devices page.
pub const SYNC_PROTOCOL_VERSION: u32 = 1;
