#!/usr/bin/env bash
# Rebuild or pull the Buzz relay image and recreate the compose relay service.
#
# Volumes (Postgres, Redis, MinIO, git) are kept. Dependencies that are not
# already running are started; they are not force-recreated.
#
# Usage:
#   ./redeploy.sh                 # local tag → docker build; registry tag → pull
#   ./redeploy.sh --build         # always build from this checkout
#   ./redeploy.sh --pull          # always pull BUZZ_IMAGE
#   ./redeploy.sh --skip-build    # recreate the relay container only
#   ./run.sh redeploy [opts]      # same command
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

compose_dir() {
  echo "${BUZZ_COMPOSE_DIR:-${SCRIPT_DIR}}"
}

usage() {
  cat <<'MSG'
Usage: ./redeploy.sh [options]

Rebuild the Buzz relay image (or pull a registry image) and recreate the
compose relay service. Postgres, Redis, MinIO, and git volumes are kept.

Options:
  --build         Force docker build from this checkout
  --pull          Force docker compose pull of BUZZ_IMAGE
  --skip-build    Recreate the relay without building or pulling
  --no-wait       Recreate without waiting for healthchecks
  -h, --help      Show this help

Image source (when neither --build nor --pull is set):
  - Local names such as buzz-local:current are built from the repo Dockerfile
  - Registry names such as ghcr.io/block/buzz:main are pulled

Environment:
  BUZZ_COMPOSE_TLS=true    Include compose.caddy.yml
  BUZZ_COMPOSE_DEV=true    Include compose.dev.yml
  BUZZ_COMPOSE_DIR         Compose directory (default: this script's directory)
MSG
}

log() { echo "==> $*"; }
die() { echo "FAIL: $*" >&2; exit 1; }

# Namespaced images (contain '/') are treated as registry references.
is_registry_image() {
  local image="${1%%@*}"
  [[ "${image}" == */* ]]
}

read_env_value() {
  local key="$1"
  local file="$2"
  local line
  line="$(grep -E "^[[:space:]]*${key}=" "${file}" | tail -n 1 || true)"
  [[ -n "${line}" ]] || return 1
  local value="${line#*=}"
  value="${value%$'\r'}"
  printf '%s' "${value}"
}

require_env() {
  local dir
  dir="$(compose_dir)"
  if [[ ! -f "${dir}/.env" ]]; then
    cat >&2 <<'MSG'
Missing deploy/compose/.env.

Run ./bootstrap-local.sh with a relay hostname and owner pubkey, or copy
.env.example to .env and replace every CHANGE_ME value. Do not start production
with generated secrets missing.
MSG
    exit 1
  fi
  if grep -Eq '^[[:space:]]*[A-Za-z_][A-Za-z0-9_]*=.*CHANGE_ME' "${dir}/.env"; then
    cat >&2 <<'MSG'
deploy/compose/.env still contains CHANGE_ME placeholders.
Generate stable secrets first; these values must not rotate on restart.
MSG
    exit 1
  fi
}

require_docker() {
  if ! command -v docker >/dev/null 2>&1; then
    die "Docker is required. Install it from https://docs.docker.com/get-docker/"
  fi
  if ! docker info >/dev/null 2>&1; then
    die "Docker daemon is not running. Start Docker and try again."
  fi
}

compose_files() {
  local files=(-f compose.yml)
  if [[ "${BUZZ_COMPOSE_TLS:-false}" == "true" ]]; then
    files+=(-f compose.caddy.yml)
  fi
  if [[ "${BUZZ_COMPOSE_DEV:-false}" == "true" ]]; then
    files+=(-f compose.dev.yml)
  fi
  printf '%s\n' "${files[@]}"
}

compose() {
  local files=()
  while IFS= read -r line; do
    files+=("${line}")
  done < <(compose_files)
  docker compose --env-file .env "${files[@]}" "$@"
}

proxy_build_args() {
  local ca_pem="${REPO_ROOT}/deploy/local/proxy-ca.pem"
  if [[ ! -f "${ca_pem}" ]] && command -v security >/dev/null 2>&1; then
    : >"${ca_pem}"
    local name
    for name in "Cloudflare Gateway CA" \
      "Service To Service AWS Native CA production G0" \
      "Corp Systems AWS Native CA production G0" \
      "Block, Inc CA G1" \
      "Square Primary Certificate Authority - G2"; do
      security find-certificate -a -c "${name}" -p /Library/Keychains/System.keychain >>"${ca_pem}" 2>/dev/null || true
    done
  fi
  if [[ -f "${ca_pem}" ]] && grep -q 'BEGIN CERTIFICATE' "${ca_pem}"; then
    printf '%s\n' --build-arg "EXTRA_CA_CERTS=deploy/local/proxy-ca.pem"
  fi
  local host_npm_reg=""
  host_npm_reg="$(pnpm config get registry 2>/dev/null || true)"
  if [[ -n "${host_npm_reg}" && "${host_npm_reg}" != *registry.npmjs.org* ]]; then
    printf '%s\n' --build-arg "NPM_REGISTRY=${host_npm_reg}"
  fi
}

build_image() {
  local image="$1"
  local sha
  sha="$(git -C "${REPO_ROOT}" rev-parse --short HEAD 2>/dev/null || echo unknown)"
  log "building ${image} from ${REPO_ROOT} (HEAD ${sha})"
  local build_cmd=(docker build -t "${image}" -f "${REPO_ROOT}/Dockerfile")
  local arg
  while IFS= read -r arg; do
    [[ -n "${arg}" ]] || continue
    build_cmd+=("${arg}")
  done < <(proxy_build_args)
  build_cmd+=("${REPO_ROOT}")
  "${build_cmd[@]}"
}

pull_image() {
  log "pulling ${1}"
  compose pull relay
}

recreate_relay() {
  local wait_flag="$1"
  if [[ "${wait_flag}" == "1" ]]; then
    log "recreating relay (volumes kept)"
    compose up -d --wait --force-recreate relay
  else
    log "recreating relay without waiting (volumes kept)"
    compose up -d --force-recreate relay
  fi
}

probe_ready() {
  log "probing /_readiness"
  if compose exec -T relay bash -ec '
    if command -v curl >/dev/null 2>&1; then
      curl -fsS http://127.0.0.1:8080/_readiness
    else
      exec 3<>/dev/tcp/127.0.0.1/8080
      printf "GET /_readiness HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n" >&3
      grep -q "200 OK" <&3
    fi
  '; then
    echo
    log "relay is ready"
    return 0
  fi
  echo >&2
  log "readiness probe failed; last relay logs:"
  compose logs --tail=80 relay >&2 || true
  die "relay did not report ready"
}

main() {
  local mode="auto"
  local wait_for_health=1

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --build | --pull | --skip-build)
        if [[ "${mode}" != "auto" ]]; then
          die "use only one of --build, --pull, or --skip-build"
        fi
        case "$1" in
          --build) mode="build" ;;
          --pull) mode="pull" ;;
          --skip-build) mode="skip" ;;
        esac
        shift
        ;;
      --no-wait)
        wait_for_health=0
        shift
        ;;
      -h | --help)
        usage
        exit 0
        ;;
      *)
        echo "Unknown option: $1" >&2
        usage >&2
        exit 1
        ;;
    esac
  done

  require_env
  require_docker

  cd "$(compose_dir)"

  local image
  image="$(read_env_value BUZZ_IMAGE .env || true)"
  image="${image:-buzz-local:current}"

  if [[ "${mode}" == "auto" ]]; then
    if is_registry_image "${image}"; then
      mode="pull"
    else
      mode="build"
    fi
  fi

  case "${mode}" in
    build) build_image "${image}" ;;
    pull) pull_image "${image}" ;;
    skip) log "skipping image build/pull; reusing ${image}" ;;
    *) die "unknown mode: ${mode}" ;;
  esac

  recreate_relay "${wait_for_health}"
  if [[ "${wait_for_health}" == "1" ]]; then
    probe_ready
  fi
  compose ps
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  main "$@"
fi
