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

## Encryption (`crates/sync`)

- Group key: 256 random bits, shown as a 24-word BIP-39 phrase for pairing. HKDF-SHA256 derives three independent values from it: the group id (16 bytes, hex; names the group on a relay or in a folder), the relay token (the relay stores only its SHA-256, and the first writer claims the group), and the encryption key.
- Device key: Ed25519, generated per device. The group key and device key live in the OS keychain.
- Everything a transport stores is a sealed item: `{v, device, key, nonce, data, sig}`. `data` is the JSON payload encrypted with XChaCha20-Poly1305 (AAD `group_id\0device`); `sig` is the device key's signature over `group_id\0device\0nonce||data`. A receiver pins each device's public key the first time it sees it and rejects items signed by another key.
- Payloads: a `Batch { ops }` of up to 400 of the sender's own ops, or a `DeviceInfo` heartbeat (`id, name, os, appVersion, protocolVersion, sentAt`), published every minute.
- Planned: removing a device rotates the group key; optional secrets sync with a separate vault key from a vault passphrase (Argon2id).

## Transports

- **Shared folder**: `<folder>/linecard-<group id>/ops/<device>/<seq:012>.json` and `devices/<device>.json`. Files are written to a temp name and renamed, each device only writes its own directory, and readers keep a per-device cursor and stop at the first missing or partial file (cloud-sync clients deliver files out of order).
- **Relay** (`relay/`): `POST/GET /v1/groups/{group}/ops` (paged by sequence number), `PUT /v1/groups/{group}/devices/{device}`, `GET /v1/groups/{group}/devices`, bearer token. The relay keeps blobs in SQLite in arrival order, up to a per-group limit.

Clients push their unsent ops, pull everything after their cursor, and repeat every 5 s (relay) or 10 s (folder), or immediately after a local edit.

## Versioning

`SYNC_PROTOCOL_VERSION` is published in each device's heartbeat. Clients must preserve fields they don't understand. A device below the group's minimum version stops pushing until updated; every client flags mismatches on the Devices page.
