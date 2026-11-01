# Deploying the Linecard relay

The relay stores and forwards end-to-end encrypted sync data. These files run it with HTTPS on a server you control. Everything it holds is ciphertext; it never sees keys, host names or passwords.

## Quick start (Docker Compose + Caddy)

You need a server with Docker, a DNS name pointing at it, and ports 80 and 443 reachable from the internet (Caddy gets its certificate from Let's Encrypt over them).

```bash
cd relay/deploy
cp .env.example .env      # set RELAY_DOMAIN
docker compose up -d
docker compose ps         # both services should be running; relay shows "healthy"
```

In Linecard: **Settings → Sync → Relay**, enter `https://<RELAY_DOMAIN>`.

| File | What it does |
| --- | --- |
| `compose.yaml` | The relay (read-only filesystem, no capabilities, reachable only through Caddy) and Caddy (HTTPS, ports 80/443). |
| `Caddyfile` | Reverse proxy with security headers and a request-size limit matching the relay's. |
| `.env.example` | The settings you may change. |

### Everyday commands

```bash
docker compose exec relay linecard-relay stats
docker compose exec relay linecard-relay account add alice --max-groups 3   # private relay: see LINECARD_RELAY_OPEN
docker compose logs -f relay
docker compose pull && docker compose up -d                                 # upgrade
```

Upgrading keeps your data: it lives in the `relay-data` volume. Keep the `caddy-data` volume too, or Caddy has to fetch new certificates.

### Backups

The relay makes a backup of its database every day into the `relay-backups` volume and keeps the newest seven (`LINECARD_RELAY_BACKUP_KEEP`). A backup is taken while the relay serves, with no downtime, and is checked after it is made.

A backup on the same disk doesn't survive losing the server, so copy them elsewhere. Everything in them is already encrypted sync data, but they also hold hashes of the groups' tokens and of account keys, so keep them private.

```bash
docker compose exec relay linecard-relay backup /backups/     # make one now
docker compose cp relay:/backups/. ./relay-backups            # copy them all to this folder (then onto another machine)
```

A nightly copy off the server, as a cron job on the host:

```bash
0 4 * * *  cd /path/to/relay/deploy && docker compose cp relay:/backups/. /mnt/offsite/linecard-relay/
```

**Restoring** replaces the database with a backup. Stop the relay first; the database it replaces is kept beside it (as `…db.before-restore-<time>`), never deleted:

```bash
docker compose stop relay
docker compose cp ./relay-backups/linecard-relay-20260101-040000.db relay:/backups/   # skip if it is already in the volume
docker compose run --rm --no-deps relay restore /backups/linecard-relay-20260101-040000.db
docker compose start relay
```

(`docker compose cp` into a stopped container works; if yours refuses, start a throwaway container with the `relay-backups` volume mounted and copy the file there.)

After a restore the relay announces a new history. Each computer notices within about half a minute, sends all of its own changes again (computers skip what they already have), and re-reads the relay from the start, so changes made after the backup was taken come back from the computers that made them. Computers running a Linecard from before this feature do not notice, so update them before relying on a restore.

## Other ways to run it

### Behind a proxy you already have

Run only the relay and let your proxy reach it. Remove the `caddy` service and publish the relay's port on the host (or join your proxy's network):

```yaml
services:
  relay:
    ports: ["127.0.0.1:8443:8443"]
```

Whatever the proxy is, it must **not time out idle responses before about 30 seconds**: devices keep a request open for up to 25 s waiting for changes. Request bodies are up to 1 MiB.

nginx:

```nginx
location / {
    proxy_pass http://127.0.0.1:8443;
    proxy_read_timeout 60s;
    proxy_buffering off;
    client_max_body_size 1100k;
}
```

Traefik (labels on the relay service; adjust the entrypoint and resolver names to yours):

```yaml
labels:
  - traefik.enable=true
  - traefik.http.routers.relay.rule=Host(`relay.example.com`)
  - traefik.http.routers.relay.entrypoints=websecure
  - traefik.http.routers.relay.tls.certresolver=letsencrypt
  - traefik.http.services.relay.loadbalancer.server.port=8443
```

Traefik's default timeouts suit the long poll.

### Cloudflare Tunnel (no open ports)

Run the relay without Caddy and add a `cloudflared` service on the same network, with the tunnel's public hostname pointing at `http://relay:8443`:

```yaml
services:
  relay:
    networks: [backend]
  tunnel:
    image: cloudflare/cloudflared:latest
    restart: unless-stopped
    command: tunnel --no-autoupdate run
    environment:
      TUNNEL_TOKEN: ${TUNNEL_TOKEN}
    networks: [backend, default]
```

Cloudflare ends the HTTPS connection, so it can see the sync tokens in transit (the data stays encrypted end to end).

### Just Docker, on a LAN

```bash
docker run -d --name linecard-relay --restart unless-stopped -p 8443:8443 \
  -v linecard-relay:/data ghcr.io/paintballrefjosh/linecard-relay
```

This speaks plain HTTP, so use it only on a network you trust (or put a VPN such as WireGuard in front).

### Without Docker (systemd)

Build or download the `linecard-relay` binary, then:

```bash
sudo useradd --system --home /var/lib/linecard-relay --create-home --shell /usr/sbin/nologin linecard
sudo install -m 755 linecard-relay /usr/local/bin/
```

`/etc/systemd/system/linecard-relay.service`:

```ini
[Unit]
Description=Linecard sync relay
After=network-online.target
Wants=network-online.target

[Service]
User=linecard
Environment=LINECARD_RELAY_ADDR=127.0.0.1:8443
Environment=LINECARD_RELAY_DB=/var/lib/linecard-relay/linecard-relay.db
Environment=LINECARD_RELAY_OPEN=true
ExecStart=/usr/local/bin/linecard-relay
Restart=on-failure
TimeoutStopSec=10
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=/var/lib/linecard-relay

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable --now linecard-relay
sudo -u linecard LINECARD_RELAY_DB=/var/lib/linecard-relay/linecard-relay.db linecard-relay stats
```

Put an HTTPS proxy in front (see above).

## Troubleshooting

- **Caddy can't get a certificate**: the DNS name must point at this server and ports 80 and 443 must be open to the internet. `docker compose logs caddy` says which check failed.
- **403 when starting sync**: this is a private relay (`LINECARD_RELAY_OPEN=false`); the first computer needs an account's access key (`docker compose exec relay linecard-relay account add <name>`). Other computers joining a group need only the phrase.
- **401**: the sync phrase doesn't match the group stored on this relay (a different phrase, or the group was started with another one).
- **Changes arrive slowly (about every 25 s)**: a proxy is cutting or buffering the long-poll. Disable response buffering and raise its read timeout to 60 s or more.
- **507**: the group is full (`LINECARD_RELAY_MAX_OPS` batches) or the account reached its group limit.
- **Clock differences between computers don't matter.**
