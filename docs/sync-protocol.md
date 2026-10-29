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
- Removing a device (or changing the phrase) rotates the group key: the device that does it posts a signed `Notice { closedBy, at }` in the old group, makes a new group key, and pushes the whole op log there. Other devices stop on the notice until given the new phrase.
- Password sync (opt-in): a `vault` object holds a random salt and a check value; the vault key is Argon2id(passphrase, salt) (64 MiB, 3 passes). Each saved password is a `secret` object `{key, label, blob}` where blob = base64(nonce || XChaCha20-Poly1305(password, AAD = key)). Each device keeps the vault key in its keychain and reconciles keychain and records both ways after every round.

## Transports

- **Shared folder**: `<folder>/linecard-<group id>/ops/<device>/<seq:012>.json` and `devices/<device>.json`. Files are written to a temp name and renamed, each device only writes its own directory, and readers keep a per-device cursor and stop at the first missing or partial file (cloud-sync clients deliver files out of order).
- **Relay** (`relay/`): `POST/GET /v1/groups/{group}/ops` (paged by sequence number), `PUT /v1/groups/{group}/devices/{device}`, `GET /v1/groups/{group}/devices`, bearer token. The relay keeps blobs in SQLite in arrival order, up to a per-group limit.

- **GitHub** (`crates/sync/src/github.rs`): the shared-folder layout in a repository. Pushes are commits through the contents API; pulls list the tree once and fetch only new blobs. Polled every 30 s.

- **This network** (`crates/sync/src/lan.rs`): each computer keeps every sealed item it has in a local relay store and serves it with the relay protocol on a random TCP port, behind the group token. A multicast beacon (239.255.42.99:47123, every 3 s) carries a hash of the group id, the device id and the port. Each computer pulls from every peer it hears (cursor per peer) and keeps what it learns, so items are forwarded.

Clients push their unsent ops, pull everything after their cursor, and repeat every 5 s (relay) or 10 s (folder), or immediately after a local edit.

## Versioning

`SYNC_PROTOCOL_VERSION` is published in each device's heartbeat. Clients must preserve fields they don't understand. A device below the group's minimum version stops pushing until updated; every client flags mismatches on the Devices page.
