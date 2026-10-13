#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_DIR}"' EXIT

# shellcheck source=deploy/compose/redeploy.sh
source "${SCRIPT_DIR}/redeploy.sh"

expect_contains() {
  local haystack="$1"
  local needle="$2"
  if [[ "${haystack}" != *"${needle}"* ]]; then
    echo "Expected to find: ${needle}" >&2
    echo "In: ${haystack}" >&2
    exit 1
  fi
}

# ── Image-name classification ────────────────────────────────────────────────
is_registry_image "ghcr.io/block/buzz:main"
is_registry_image "ghcr.io/block/buzz@sha256:deadbeef"
! is_registry_image "buzz-local:current"
! is_registry_image "buzz"
is_registry_image "block/buzz:main"

# ── Help does not require .env or Docker ─────────────────────────────────────
help_out="$("${SCRIPT_DIR}/redeploy.sh" --help)"
expect_contains "${help_out}" "Rebuild the Buzz relay image"
expect_contains "$("${SCRIPT_DIR}/run.sh" help)" "redeploy"
expect_contains "$("${SCRIPT_DIR}/run.sh" redeploy --help)" "Rebuild the Buzz relay image"

# ── Missing / placeholder .env ───────────────────────────────────────────────
export BUZZ_COMPOSE_DIR="${TEST_DIR}"
cp "${SCRIPT_DIR}/compose.yml" "${TEST_DIR}/compose.yml"
cp "${SCRIPT_DIR}/.env.example" "${TEST_DIR}/.env.example"

missing_env="$("${SCRIPT_DIR}/redeploy.sh" --skip-build 2>&1 || true)"
expect_contains "${missing_env}" "Missing deploy/compose/.env"

cp "${SCRIPT_DIR}/.env.example" "${TEST_DIR}/.env"
placeholder_env="$("${SCRIPT_DIR}/redeploy.sh" --skip-build 2>&1 || true)"
expect_contains "${placeholder_env}" "CHANGE_ME"

# ── Conflicting flags ────────────────────────────────────────────────────────
conflict_out="$("${SCRIPT_DIR}/redeploy.sh" --build --pull 2>&1 || true)"
expect_contains "${conflict_out}" "use only one of --build, --pull, or --skip-build"

unknown_out="$("${SCRIPT_DIR}/redeploy.sh" --explode 2>&1 || true)"
expect_contains "${unknown_out}" "Unknown option: --explode"

# ── Fake docker: skip-build recreates the relay without a docker build ───────
write_env() {
  local image="$1"
  cat >"${TEST_DIR}/.env" <<EOF
BUZZ_IMAGE=${image}
BUZZ_DOMAIN=relay.example.ts.net
RELAY_URL=wss://relay.example.ts.net
BUZZ_MEDIA_BASE_URL=https://relay.example.ts.net/media
BUZZ_MEDIA_SERVER_DOMAIN=relay.example.ts.net
BUZZ_CORS_ORIGINS=https://relay.example.ts.net
BUZZ_REQUIRE_AUTH_TOKEN=true
BUZZ_REQUIRE_RELAY_MEMBERSHIP=true
BUZZ_REQUIRE_MEDIA_GET_AUTH=true
BUZZ_ALLOW_NIP_OA_AUTH=true
BUZZ_AUTO_MIGRATE=true
BUZZ_GIT_CONFORMANCE_PROBE=true
RELAY_OWNER_PUBKEY=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
BUZZ_RELAY_PRIVATE_KEY=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
BUZZ_GIT_HOOK_HMAC_SECRET=cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
POSTGRES_DB=buzz
POSTGRES_USER=buzz
POSTGRES_PASSWORD=dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd
REDIS_PASSWORD=eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee
BUZZ_S3_ACCESS_KEY=buzzffffffffffffffffffffffffffffffff
BUZZ_S3_SECRET_KEY=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
BUZZ_S3_BUCKET=buzz-media
BUZZ_S3_ADDRESSING_STYLE=path
BUZZ_HTTP_PORT=3000
EOF
}

mkdir -p "${TEST_DIR}/bin"
cat >"${TEST_DIR}/bin/docker" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
log="${DOCKER_LOG:?DOCKER_LOG is required}"
printf '%s\n' "$*" >>"${log}"
case "${1:-}" in
  info)
    exit 0
    ;;
  build)
    echo "Successfully built fake-image"
    exit 0
    ;;
  compose)
    shift
    if [[ "$*" == *exec* ]]; then
      echo '{"status":"ready"}'
      exit 0
    fi
    if [[ "$*" == *ps* ]]; then
      echo "NAME                STATUS"
      echo "buzz-prod-relay-1   running (healthy)"
      exit 0
    fi
    if [[ "$*" == *logs* ]]; then
      echo "fake relay logs"
      exit 0
    fi
    exit 0
    ;;
  *)
    echo "unexpected docker invocation: $*" >&2
    exit 1
    ;;
esac
EOF
chmod +x "${TEST_DIR}/bin/docker"
export PATH="${TEST_DIR}/bin:${PATH}"

write_env "buzz-local:current"
export DOCKER_LOG="${TEST_DIR}/docker-skip.log"
: >"${DOCKER_LOG}"
"${SCRIPT_DIR}/redeploy.sh" --skip-build >/dev/null
if grep -q '^build ' "${DOCKER_LOG}"; then
  echo "skip-build must not invoke docker build" >&2
  cat "${DOCKER_LOG}" >&2
  exit 1
fi
if grep -q ' pull ' "${DOCKER_LOG}"; then
  echo "skip-build must not invoke docker compose pull" >&2
  cat "${DOCKER_LOG}" >&2
  exit 1
fi
grep -q -- '--force-recreate relay' "${DOCKER_LOG}"
grep -q -- '--wait' "${DOCKER_LOG}"

export DOCKER_LOG="${TEST_DIR}/docker-no-wait.log"
: >"${DOCKER_LOG}"
"${SCRIPT_DIR}/redeploy.sh" --skip-build --no-wait >/dev/null
if grep -q -- '--wait' "${DOCKER_LOG}"; then
  echo "--no-wait must not pass --wait to compose up" >&2
  cat "${DOCKER_LOG}" >&2
  exit 1
fi
if grep -q 'exec ' "${DOCKER_LOG}"; then
  echo "--no-wait must not probe readiness" >&2
  cat "${DOCKER_LOG}" >&2
  exit 1
fi

# Auto mode: local image → docker build
export DOCKER_LOG="${TEST_DIR}/docker-build.log"
: >"${DOCKER_LOG}"
"${SCRIPT_DIR}/redeploy.sh" >/dev/null
grep -q '^build ' "${DOCKER_LOG}"
grep -q -- '-t buzz-local:current' "${DOCKER_LOG}"
grep -q -- '--force-recreate relay' "${DOCKER_LOG}"

# Auto mode: registry image → compose pull
write_env "ghcr.io/block/buzz:main"
export DOCKER_LOG="${TEST_DIR}/docker-pull.log"
: >"${DOCKER_LOG}"
"${SCRIPT_DIR}/redeploy.sh" >/dev/null
if grep -q '^build ' "${DOCKER_LOG}"; then
  echo "registry image must not invoke docker build by default" >&2
  cat "${DOCKER_LOG}" >&2
  exit 1
fi
grep -q ' pull relay' "${DOCKER_LOG}"

# Forced build of a registry tag still runs docker build
export DOCKER_LOG="${TEST_DIR}/docker-force-build.log"
: >"${DOCKER_LOG}"
"${SCRIPT_DIR}/redeploy.sh" --build >/dev/null
grep -q '^build ' "${DOCKER_LOG}"
grep -q -- '-t ghcr.io/block/buzz:main' "${DOCKER_LOG}"

# TLS override is passed through to compose
export BUZZ_COMPOSE_TLS=true
cp "${SCRIPT_DIR}/compose.caddy.yml" "${TEST_DIR}/compose.caddy.yml"
export DOCKER_LOG="${TEST_DIR}/docker-tls.log"
: >"${DOCKER_LOG}"
write_env "buzz-local:current"
"${SCRIPT_DIR}/redeploy.sh" --skip-build >/dev/null
grep -q -- '-f compose.caddy.yml' "${DOCKER_LOG}"
unset BUZZ_COMPOSE_TLS

echo "redeploy tests passed"
