# Self-hosted deployment

The vtc-mobile community runs on the existing Tailscale server through Docker Compose.
The relay URL is `wss://vtc.taildae6a9.ts.net` and the HTTPS URL is `https://vtc.taildae6a9.ts.net/`.
Clients must have access to the same tailnet.
GoClaw remains on port 18790.

## Services and configuration

The deployment directory on the server is `/home/vtcm/buzz-relay/vtc-mobile`.
Run Compose commands from that directory.
The `compose.yml` file defines the relay, PostgreSQL, and authenticated Redis.
Only relay ports 3080 and 3081 bind to the server loopback address.
Tailscale Serve forwards HTTPS port 443 to `http://127.0.0.1:3080`.
The Compose network has MTU 1280 because the previous bridge MTU caused lost TLS segments on the Wasabi path.

Private `.env` configuration holds database, Redis, relay, and S3 credentials.
Do not commit or print that file.
Set `BUZZ_S3_ENDPOINT`, `BUZZ_S3_REGION`, `BUZZ_S3_BUCKET`, `BUZZ_S3_ACCESS_KEY`, and `BUZZ_S3_SECRET_KEY` for the existing Wasabi service.
Set `BUZZ_S3_PREFIX=buzz-media` to place media and Git objects under that prefix in `game-mobile-registry`.
Logical media URLs and database keys do not include this physical prefix.
MinIO is not required.

## Operations

```bash
docker compose up -d
docker compose ps
curl -fsS http://127.0.0.1:3081/_readiness
```

Run explicit migrations before starting an image that requires them.
The deployed relay keeps automatic migrations disabled.

```bash
docker compose run --rm --no-deps --entrypoint buzz-admin relay migrate
```

Build the desktop and remote agent binaries from the same mbuzz fork.
The remote `buzz-goclaw.service` runs `buzz-acp` on the server and connects to this relay.
Its GoClaw ACP subprocess connects to the local GoClaw gateway and uses the game-studio tenant and fox-spirit agent.
The desktop needs Buzz and tailnet access; it does not need GoClaw installed.

## Backup and rollback

The server `backups/` directory holds private environment backups, PostgreSQL dumps, and the previous remote agent launcher and identity configuration.
The Mac backup is `/Users/dale/Desktop/Buzz-backups/261006-before-vtc-mobile`.
Stop Buzz before restoring its app or user data.
Stop the remote agent before restoring its saved launcher and relay identity configuration, then restart `buzz-goclaw.service`.

To stop this relay, run `docker compose down` without `-v` to preserve all named volumes.
To remove only its HTTPS route, run `tailscale serve --https=443 off`.
Do not reset unrelated Tailscale routes.
Restore a database dump only into a stopped, selected deployment after making a new backup of its current database.
