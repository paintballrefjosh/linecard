# Sync protocol (draft, version 1)

The engine lives in `crates/core-sync`; transports only move ciphertext.

## Objects and ops

- Every synced object (folder, connection, credential, profile, device…) has a UUIDv7 id and a set of named fields.
- Every change is an `Op`: `{ id: Hlc, object, object_kind, action, changes: {field: value}, prev: {field: value|null} }`.
- `id` is a hybrid logical clock `(wall_ms, counter, device)`. It is unique per op and totally ordered.
- `action` is `create | update | delete | restore | revert{of}`. It only affects how the change is shown; the merge treats every op as field writes.
- Deletes write the reserved field `_deleted = true` (a tombstone). Tombstones are garbage-collected only after every device's cursor has passed them.

## Merge

For each field in `changes`, the op wins if its `id` is greater than the id of the op that last wrote that field (last-writer-wins per field). This is commutative and idempotent, so all devices converge regardless of delivery order. Consequences:

- Two devices editing different fields of one connection both keep their edits.
- A delete racing an edit: the object ends up deleted, but the edit is kept, so restoring brings back the edited version.

## Revert

A revert is a new op whose `changes` are the target op's `prev` values (a missing `_deleted` means "did not exist", so reverting a create deletes the object). Its own `prev` captures the current values, so a revert can be reverted.

## Change tracking (per device, not synced)

Remote ops whose writes change a visible value become pending changes: `added`, `edited`, `moved` (only `folder`/`parent`/`order` changed), `deleted`, `restored`, `reverted`. Added-then-deleted before acknowledgement collapses to nothing. A local edit clears the pending highlight for that object.

## Encryption (Phase 2)

- Group key: 256-bit, shown as a 24-word phrase / QR for pairing.
- Op batches: XChaCha20-Poly1305 with the group key; each batch signed by the device's Ed25519 key.
- Removing a device rotates the group key.
- Optional secrets sync uses a separate vault key from a vault passphrase (Argon2id).

## Versioning

`SYNC_PROTOCOL_VERSION` is published in each device's heartbeat. Clients must preserve fields they don't understand. A device below the group's minimum version stops pushing until updated; every client flags mismatches on the Devices page.
