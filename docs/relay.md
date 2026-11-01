# Running a relay

The relay lets your computers sync over the internet. It stores only end-to-end encrypted blobs: it never sees connection names, hosts or passwords.

## Start it

The easy way is the Docker Compose bundle in [`relay/deploy`](../relay/deploy/README.md): it runs the relay and Caddy, which gets a Let's Encrypt certificate for your domain automatically.

```bash
cd relay/deploy
cp .env.example .env      # set RELAY_DOMAIN to the DNS name of your server
docker compose up -d
```

Or just the relay, for a network you trust:

```bash
docker run -d --name linecard-relay -p 8443:8443 -v linecard-relay:/data ghcr.io/paintballrefjosh/linecard-relay
```

Put that behind HTTPS (Caddy, Traefik, nginx, a Cloudflare Tunnel; the deploy README has examples), then in Linecard choose **Settings → Sync → Relay** and enter its address, e.g. `https://relay.example.com`.

The image is small (a static binary on a distroless base): it has no shell, runs as a non-root user (65532), is built for `linux/amd64` and `linux/arm64`, and carries a built-in health check (`linecard-relay health`). Tags are `:X.Y.Z`, `:X.Y` and `:latest`; a published version is never re-tagged. `docker stop` ends it cleanly within a few seconds. It also runs with `--read-only --cap-drop ALL --security-opt no-new-privileges:true`; mount the `/data` volume (a bind mount must be owned by uid 65532). Admin commands work as before: `docker exec linecard-relay linecard-relay stats`.

Changes reach the other computers within about a second: each one keeps a request open on the relay, which answers as soon as something new arrives.

## Settings

| Variable | Default | |
| --- | --- | --- |
| `LINECARD_RELAY_ADDR` | `0.0.0.0:8443` | Listen address |
| `LINECARD_RELAY_DB` | `/data/linecard-relay.db` (Docker) | SQLite database |
| `LINECARD_RELAY_MAX_OPS` | `200000` | Batches kept per sync group |
| `LINECARD_RELAY_OPEN` | `true` | `false` makes the relay private (accounts only) |
| `LINECARD_RELAY_MAX_GROUP_BYTES` | `256MiB` | Storage per sync group (`1GiB`, `500MB`, `0` = no limit); writes beyond it fail with 507 |
| `LINECARD_RELAY_IDLE_DAYS` | `0` (never) | Delete groups with no change and no computer reporting in for this many days |

## Private relays and accounts

An open relay lets anyone with a sync phrase start a group. To limit it to people you choose, run it with `LINECARD_RELAY_OPEN=false` and create an account for each person:

```bash
docker exec linecard-relay linecard-relay account add alice --max-groups 3
```

This prints an access key (`lcr_…`), shown only once. Alice enters it under **Settings → Sync → Access key** when she starts her sync group. Her other computers join with the 24-word phrase alone.

## Storage limits and cleanup

A relay left running for months stays bounded:

- **Per-group quota.** A sync group may hold `LINECARD_RELAY_MAX_GROUP_BYTES` (256 MiB by default) and `LINECARD_RELAY_MAX_OPS` batches. Beyond either, writes fail with `507` and the app shows a sync error; nothing already stored is dropped, and reading keeps working.
- **Stale computers.** A computer that hasn't reported in for 90 days drops off the group's device list.
- **Idle groups.** With `LINECARD_RELAY_IDLE_DAYS` set (off by default), a group with no change and no computer reporting in for that long is deleted, with its data. The check runs when the relay starts and then daily, and logs each removal. Preview what it would remove with `linecard-relay group list --idle 90`.
- **Disk space.** Deleting data frees space inside the database file; `linecard-relay vacuum` hands it back to the system (writes pause for a moment while it runs).
- **Upgrades.** The database records its schema version and upgrades itself when a newer relay opens it. An older relay refuses a database from a newer one rather than damaging it.

Compaction (shrinking a long history to a snapshot) needs a change to the sync protocol and isn't built yet.

## Backups and restoring

The relay's whole state is one SQLite file. Make a copy of it at any time, while the relay serves, with no downtime:

```bash
docker exec linecard-relay linecard-relay backup /data/backups/      # a folder: the file is named for the time
docker exec linecard-relay linecard-relay backup /data/relay.bak.db  # or a file
```

Every copy is checked after it is made (integrity and schema) and appears only when it is good. For a daily copy without cron, set `LINECARD_RELAY_BACKUP_DIR` (the Compose bundle does, to a volume) and `LINECARD_RELAY_BACKUP_KEEP` (default 7): the relay writes `linecard-relay-YYYYMMDD-HHMMSS.db` once a day and deletes the oldest beyond that. Copy them off the server too.

To restore, **stop the relay**, then:

```bash
linecard-relay restore /path/to/linecard-relay-20260101-040000.db
```

It refuses damaged files, files from a newer relay, and (when it can tell) a relay that is still running. The database it replaces is kept beside it as `…before-restore-<time>`.

Restoring sends the relay back in time, so it gives itself a new history id. Linecard on each computer notices within about half a minute, re-sends its own changes and re-reads the relay from the start; changes made after the backup come back from the computers that made them. (Only Linecard versions that include this feature notice; update the computers before relying on a restore.)

Backups contain only encrypted sync data, plus hashes of the groups' tokens and accounts' keys: keep them private.

## Admin commands

Run them where the database lives (`docker exec linecard-relay …`):

```text
linecard-relay account add <name> [--max-groups N]   create an account, print its access key
linecard-relay account list
linecard-relay account remove <name> [--with-groups]
linecard-relay group list [--idle [DAYS]]          id, owner, batches, size, devices, last write and last seen; --idle: only quiet groups
linecard-relay vacuum                                 give deleted data's disk space back
linecard-relay group remove <group-id>
linecard-relay stats
```

Removing an account stops it from starting new groups; `--with-groups` also deletes the data of the groups it started.
