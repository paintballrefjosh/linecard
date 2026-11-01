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

## Private relays and accounts

An open relay lets anyone with a sync phrase start a group. To limit it to people you choose, run it with `LINECARD_RELAY_OPEN=false` and create an account for each person:

```bash
docker exec linecard-relay linecard-relay account add alice --max-groups 3
```

This prints an access key (`lcr_…`), shown only once. Alice enters it under **Settings → Sync → Access key** when she starts her sync group. Her other computers join with the 24-word phrase alone.

## Admin commands

Run them where the database lives (`docker exec linecard-relay …`):

```text
linecard-relay account add <name> [--max-groups N]   create an account, print its access key
linecard-relay account list
linecard-relay account remove <name> [--with-groups]
linecard-relay group list                             id, owner, size, devices, last write
linecard-relay group remove <group-id>
linecard-relay stats
```

Removing an account stops it from starting new groups; `--with-groups` also deletes the data of the groups it started.
