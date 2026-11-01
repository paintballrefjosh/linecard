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

### HTTPS without a proxy

If you'd rather not run a proxy, the relay can serve HTTPS itself. Give it a certificate chain and its private key as PEM files (certbot's `fullchain.pem` and `privkey.pem`, for example):

```bash
docker run -d --name linecard-relay -p 443:8443 -v linecard-relay:/data \
  -v /etc/letsencrypt/live/relay.example.com:/tls:ro \
  -e LINECARD_RELAY_TLS_CERT_FILE=/tls/fullchain.pem -e LINECARD_RELAY_TLS_KEY_FILE=/tls/privkey.pem \
  ghcr.io/paintballrefjosh/linecard-relay
```

Both settings are needed; with only one, or with unreadable files, the relay refuses to start rather than serve plain HTTP. When the files are renewed it notices within a minute and uses the new certificate (a broken file is ignored, with a warning, and the old certificate stays in use). The relay speaks HTTP/1.1 over TLS 1.2 and 1.3. It doesn't get certificates by itself; use certbot or similar, or the Compose bundle's Caddy if you want that handled. In Docker, Let's Encrypt's `live/` files are symlinks into `archive/`, so mount the whole `/etc/letsencrypt` folder read-only and point the settings at the `live/…` paths inside it.

The image is small (a static binary on a distroless base): it has no shell, runs as a non-root user (65532), is built for `linux/amd64` and `linux/arm64`, and carries a built-in health check (`linecard-relay health`). Tags are `:X.Y.Z`, `:X.Y` and `:latest`; a published version is never re-tagged. `docker stop` ends it cleanly within a few seconds. It also runs with `--read-only --cap-drop ALL --security-opt no-new-privileges:true`; mount the `/data` volume (a bind mount must be owned by uid 65532). Admin commands work as before: `docker exec linecard-relay linecard-relay stats`.

Changes reach the other computers within about a second. Each computer keeps a WebSocket open to the relay (`/v1/groups/{group}/ws`), and the relay says so the moment something new arrives. If a proxy or an older relay doesn't allow WebSockets, Linecard falls back to holding a request open (the long-poll) and tries the socket again after ten minutes. Caddy, Traefik and Cloudflare Tunnel pass WebSockets through; for nginx, forward the `Upgrade` and `Connection` headers or just leave it on the fallback. A socket uses the same per-address and per-group cap as a held request.

## Settings

| Variable | Default | |
| --- | --- | --- |
| `LINECARD_RELAY_ADDR` | `0.0.0.0:8443` | Listen address |
| `LINECARD_RELAY_DB` | `/data/linecard-relay.db` (Docker) | SQLite database |
| `LINECARD_RELAY_MAX_OPS` | `200000` | Batches kept per sync group |
| `LINECARD_RELAY_OPEN` | `true` | `false` makes the relay private (accounts only) |
| `LINECARD_RELAY_MAX_GROUP_BYTES` | `256MiB` | Storage per sync group (`1GiB`, `500MB`, `0` = no limit); writes beyond it fail with 507 |
| `LINECARD_RELAY_IDLE_DAYS` | `0` (never) | Delete groups with no change and no computer reporting in for this many days |
| `LINECARD_RELAY_RATE` | `20/s, burst 100` | Requests allowed per client address and per sync group (`off` = none) |
| `LINECARD_RELAY_TRUST_PROXY` | `false` | Read the client address from `X-Forwarded-For` (only behind your own proxy) |
| `LINECARD_RELAY_LOG` | `info` | Log level |
| `LINECARD_RELAY_LOG_FORMAT` | `text` | `text` or `json` |
| `LINECARD_RELAY_METRICS_ADDR` | off | Serve Prometheus `/metrics` here, apart from the public port |
| `LINECARD_RELAY_BACKUP_DIR` | off | Folder for daily backups |
| `LINECARD_RELAY_BACKUP_KEEP` | `7` | How many daily backups to keep |
| `LINECARD_RELAY_TLS_CERT_FILE`, `LINECARD_RELAY_TLS_KEY_FILE` | off | PEM certificate chain and private key: serve HTTPS directly (both or neither) |

## Limits and abuse protection

A relay on the internet gets scanned and guessed at. It protects itself, and none of this needs the data (everything it stores is ciphertext):

- **Request rate.** Each client address, and each sync group, may make `LINECARD_RELAY_RATE` requests (default `20/s, burst 100`; `off` for none). Past it the relay answers `429` with a `Retry-After` header, and Linecard waits that long before trying again. `/health` and `/ready` are never limited.
- **Wrong tokens.** A group's token is 256 bits derived from its phrase, so guessing one is hopeless; the relay still doesn't let an address try. After 20 wrong tokens within five minutes, that address gets `429` for five minutes (and the relay logs it, with the group id shortened).
- **Waiting requests.** Each computer holds one long request open to hear about changes quickly. At most 32 may be held per address and per group; further ones get `429`.
- **Time.** Every request but that wait is cut off after 30 seconds. Blobs are limited to 1 MiB.

**Behind a proxy** (Caddy, Traefik, nginx, a tunnel), every request arrives from the proxy's address, so the relay can't tell clients apart and its limits would apply to everyone together. Set `LINECARD_RELAY_TRUST_PROXY=true` so it reads the client address from the last `X-Forwarded-For` entry, which the proxy adds. Do that only when nothing but your proxy can reach the relay (the Compose bundle is set up this way). Without a proxy leave it `false`: anyone can write that header.

## Logs and metrics

The relay logs one line per request: method, route (with `{group}` in place of the id), status, time in milliseconds, the client address, and the first eight characters of the group id. It never logs tokens, access keys or blobs. Set `LINECARD_RELAY_LOG` (`info` by default) and `LINECARD_RELAY_LOG_FORMAT=json` for a log collector. Starting, stopping, limits, daily cleanup and backups are logged too.

Two probes for orchestrators: `/health` (the process answers) and `/ready` (the database answers and its schema is the expected one).

For Prometheus, set `LINECARD_RELAY_METRICS_ADDR` (for example `127.0.0.1:9100`, or `0.0.0.0:9100` inside a container) and scrape `/metrics` there. It's a separate port on purpose: keep it off the public internet. You get request counts and latency by route and status, requests turned away by each limit, waiting requests, and what's stored (groups, accounts, batches, bytes, devices, database size). No label contains a group id.

`linecard-relay config` prints every setting, the value in effect, whether the environment set it, and what it does; wrong values are reported with the default that replaced them.

## Private relays and accounts

An open relay lets anyone with a sync phrase start a group. To limit it to people you choose, run it with `LINECARD_RELAY_OPEN=false` and create an account for each person:

```bash
docker exec linecard-relay linecard-relay account add alice --max-groups 3
```

This prints an access key (`lcr_…`), shown only once. Alice enters it under **Settings → Sync → Access key** when she starts her sync group. Her other computers join with the 24-word phrase alone.

If a key leaks or someone leaves, replace it:

```bash
docker exec linecard-relay linecard-relay account rotate alice
```

The new key is printed once and the old one stops working at once, for starting new groups. Groups Alice already started keep syncing: her computers reach them with the group's own token, not the key, so nothing needs re-entering. Only starting a *new* group needs the new key. To also cut off a leaked group, remove it with `group remove`.

## Storage limits and cleanup

A relay left running for months stays bounded:

- **Per-group quota.** A sync group may hold `LINECARD_RELAY_MAX_GROUP_BYTES` (256 MiB by default) and `LINECARD_RELAY_MAX_OPS` batches. Beyond either, writes fail with `507` and the app shows a sync error; nothing already stored is dropped, and reading keeps working.
- **Stale computers.** A computer that hasn't reported in for 90 days drops off the group's device list.
- **Idle groups.** With `LINECARD_RELAY_IDLE_DAYS` set (off by default), a group with no change and no computer reporting in for that long is deleted, with its data. The check runs when the relay starts and then daily, and logs each removal. Preview what it would remove with `linecard-relay group list --idle 90`.
- **Disk space.** Deleting data frees space inside the database file; `linecard-relay vacuum` hands it back to the system (writes pause for a moment while it runs).
- **Upgrades.** The database records its schema version and upgrades itself when a newer relay opens it. An older relay refuses a database from a newer one rather than damaging it.

Compaction (shrinking a long history to a snapshot) needs a change to the sync protocol and isn't built yet.

### Compaction

A sync group keeps every batch that was ever pushed, which is mostly superseded edits. Linecard trims this itself: when a group holds 500 or more batches, one computer that is fully caught up (the one with the lowest id of those seen in the past week, at most once an hour) replaces the old batches with a snapshot of the current state. The relay does this in one step, only accepts a snapshot that is smaller than what it replaces, and still never reads any of it. Other computers, including ones that were behind and ones that join later, end up with the same library. The one thing lost is the ability to revert a change that was compacted before a computer saw it. Nothing to configure; `linecard-relay group list` shows the batch counts shrinking. Details: [sync-protocol.md](sync-protocol.md#compaction-relay-only-draft).
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
linecard-relay account rotate <name>                   new access key; the old one stops working (its groups keep syncing)
linecard-relay account list
linecard-relay account remove <name> [--with-groups]
linecard-relay group list [--idle [DAYS]]          id, owner, batches, size, devices, last write and last seen; --idle: only quiet groups
linecard-relay vacuum                                 give deleted data's disk space back
linecard-relay group remove <group-id>
linecard-relay config                                 every setting: value in effect, set or default, what it does
linecard-relay stats                                  totals, storage per account, largest and quietest groups
```

Removing an account stops it from starting new groups; `--with-groups` also deletes the data of the groups it started.

## Verifying the image

Release images are signed with [cosign](https://github.com/sigstore/cosign) (keyless: the signature names the release workflow that built them) and carry an SBOM and build provenance. To check one before running it:

```bash
cosign verify ghcr.io/paintballrefjosh/linecard-relay:X.Y.Z \
  --certificate-identity-regexp '^https://github.com/paintballrefjosh/linecard/\.github/workflows/release\.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
docker buildx imagetools inspect ghcr.io/paintballrefjosh/linecard-relay:X.Y.Z --format '{{ json .SBOM }}'
```
