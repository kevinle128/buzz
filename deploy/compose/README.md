# Buzz Docker Compose deployment

This is the single-node/VPS deployment bundle. It is intentionally separate from
the root `docker-compose.yml`, which remains local development infrastructure.

## Quick start

For a local relay that uses a Tailscale Funnel, run these commands:

```bash
cd deploy/compose
export RELAY_OWNER_PUBKEY='<64-character-hex-pubkey>'
./run.sh bootstrap-local my-server.example.ts.net "${RELAY_OWNER_PUBKEY}"
./redeploy.sh
tailscale funnel --bg 3000
```

Get the Tailscale host name from the target computer.
The bootstrap command creates `.env` with mode `0600` and does not overwrite an existing file.
The `.env` file contains secrets and is not stored in Git.

For a manual setup, run these commands:

```bash
cd deploy/compose
cp .env.example .env
$EDITOR .env       # replace every CHANGE_ME value
./run.sh start
```

For a public VPS with automatic Let's Encrypt certificates:

```bash
cd deploy/compose
BUZZ_COMPOSE_TLS=true ./run.sh start
```

The bootstrap command generates stable relay, database, Redis, and S3 secrets.
It requires an existing owner public key and does not handle the owner private key.

## Production notes

- Requires Docker Compose v2.24.4 or newer; the TLS override uses Compose's
  `!reset` tag to remove the direct relay port when Caddy terminates HTTPS.
- Default `BUZZ_IMAGE` tracks `ghcr.io/block/buzz:main` for early testing. Pin it to `ghcr.io/block/buzz:sha-<7>` or a semver release tag for production once available.
- Keep `BUZZ_RELAY_PRIVATE_KEY`, `BUZZ_GIT_HOOK_HMAC_SECRET`, database/Redis,
  and S3 secrets stable across restarts.
- Back up `.env` separately because a Git checkout does not contain it.
- `RELAY_OWNER_PUBKEY` is intentionally not prefixed with `BUZZ_`; it must be a
  64-character hex Nostr pubkey when closed relay mode is enabled.
- `BUZZ_AUTO_MIGRATE` is opt-in. Set `BUZZ_AUTO_MIGRATE=true` or run
  `buzz-admin migrate` before starting the relay when bootstrapping a fresh
  database. Auto-migration requires an image that includes embedded SQLx
  migrations.
- The stack uses Postgres, Redis, MinIO, and a git data volume because
  those are real Buzz dependencies today. Minimal mode can simplify this later.
- Mobile push remains off by default. To use the public gateway, keep the
  template's explicit `BUZZ_PUSH_GATEWAY_DELIVERY_URL` and set
  `BUZZ_PUSH_ENABLED=true`. To use another gateway, replace the exact HTTPS
  `/v1/deliveries/apns` URL before enabling push.
- The bundled Compose stack fixes the relay endpoint to `http://minio:9000` and
  `BUZZ_S3_ADDRESSING_STYLE=path`: Docker DNS resolves `minio`, not
  `<bucket>.minio`. It is not configurable for an external S3 provider through
  `.env`; use the Helm chart or a custom Compose configuration for providers
  such as new Railway Storage Buckets that require `virtual` addressing.

## Re-deploy

Rebuild the relay from this checkout and recreate the relay container. Postgres,
Redis, MinIO, and git volumes are kept:

```bash
cd deploy/compose
./redeploy.sh
```

`./run.sh redeploy` is the same command.

- Local image names such as `buzz-local:current` (the bootstrap default) are
  built from the repo `Dockerfile`.
- Registry names such as `ghcr.io/block/buzz:main` are pulled.
- `--build` forces a source build, `--pull` forces a registry pull, and
  `--skip-build` recreates the relay with the image already on the host.

For a published-image upgrade of the whole stack, `./run.sh upgrade` still
pulls every service and restarts.

Run `./run.sh backup-hint` for the backup checklist.

## Validation

Before sharing an install link publicly, verify a fresh install with:

```bash
cd deploy/compose
cp .env.example .env
$EDITOR .env
./run.sh config
./run.sh start
curl -fsS "http://127.0.0.1:$(grep -E '^BUZZ_HTTP_PORT=' .env | cut -d= -f2-)/_liveness"
./run.sh status
```
