#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_EXAMPLE="${SCRIPT_DIR}/.env.example"
ENV_FILE="${SCRIPT_DIR}/.env"

usage() {
  cat <<'MSG'
Usage: ./bootstrap-local.sh <relay-hostname> <owner-pubkey-hex>

Example:
  ./bootstrap-local.sh my-server.example.ts.net '<64-character-hex-pubkey>'

The command creates deploy/compose/.env with mode 0600.
It does not overwrite an existing .env file.
MSG
}

valid_hostname() {
  local hostname="$1"
  local label
  local -a labels

  [[ -n "${hostname}" ]] || return 1
  [[ ${#hostname} -le 253 ]] || return 1
  [[ "${hostname}" != .* && "${hostname}" != *. && "${hostname}" != *..* ]] || return 1
  IFS='.' read -r -a labels <<<"${hostname}"
  for label in "${labels[@]}"; do
    [[ "${label}" =~ ^[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?$ ]] || return 1
  done
}

if [[ $# -ne 2 ]]; then
  usage >&2
  exit 1
fi

relay_hostname="$1"
owner_pubkey="$2"

if ! valid_hostname "${relay_hostname}"; then
  echo "Invalid relay hostname: ${relay_hostname}" >&2
  exit 1
fi

if [[ ! "${owner_pubkey}" =~ ^[0-9A-Fa-f]{64}$ ]]; then
  echo "The owner pubkey must contain 64 hexadecimal characters." >&2
  exit 1
fi

if [[ -e "${ENV_FILE}" ]]; then
  echo "Refusing to overwrite ${ENV_FILE}." >&2
  exit 1
fi

if [[ ! -f "${ENV_EXAMPLE}" ]]; then
  echo "Missing ${ENV_EXAMPLE}." >&2
  exit 1
fi

if ! command -v openssl >/dev/null 2>&1; then
  echo "OpenSSL is required to generate relay secrets." >&2
  exit 1
fi

umask 077
relay_private_key="$(openssl rand -hex 32)"
git_hook_secret="$(openssl rand -hex 32)"
postgres_password="$(openssl rand -hex 32)"
redis_password="$(openssl rand -hex 32)"
s3_access_key="buzz$(openssl rand -hex 12)"
s3_secret_key="$(openssl rand -hex 32)"
owner_pubkey_lower="$(printf '%s' "${owner_pubkey}" | tr '[:upper:]' '[:lower:]')"
temporary_env="$(mktemp "${SCRIPT_DIR}/.env.tmp.XXXXXX")"
trap 'rm -f "${temporary_env}"' EXIT

awk \
  -v host="${relay_hostname}" \
  -v owner="${owner_pubkey_lower}" \
  -v relay_key="${relay_private_key}" \
  -v hook_secret="${git_hook_secret}" \
  -v postgres_secret="${postgres_password}" \
  -v redis_secret="${redis_password}" \
  -v access_key="${s3_access_key}" \
  -v s3_secret="${s3_secret_key}" '
BEGIN {
  values["BUZZ_IMAGE"] = "buzz-local:current"
  values["BUZZ_DOMAIN"] = host
  values["RELAY_URL"] = "wss://" host
  values["BUZZ_MEDIA_BASE_URL"] = "https://" host "/media"
  values["BUZZ_MEDIA_SERVER_DOMAIN"] = host
  values["BUZZ_CORS_ORIGINS"] = "https://" host
  values["RELAY_OWNER_PUBKEY"] = owner
  values["BUZZ_RELAY_PRIVATE_KEY"] = relay_key
  values["BUZZ_GIT_HOOK_HMAC_SECRET"] = hook_secret
  values["POSTGRES_PASSWORD"] = postgres_secret
  values["REDIS_PASSWORD"] = redis_secret
  values["BUZZ_S3_ACCESS_KEY"] = access_key
  values["BUZZ_S3_SECRET_KEY"] = s3_secret
}
{
  separator = index($0, "=")
  key = separator ? substr($0, 1, separator - 1) : ""
  if (key in values) {
    print key "=" values[key]
    next
  }
  print
}
' "${ENV_EXAMPLE}" >"${temporary_env}"

if grep -Eq '^[[:space:]]*[A-Za-z_][A-Za-z0-9_]*=.*CHANGE_ME' "${temporary_env}"; then
  echo "The generated environment still contains CHANGE_ME values." >&2
  exit 1
fi

chmod 600 "${temporary_env}"
if ! ln "${temporary_env}" "${ENV_FILE}"; then
  echo "Refusing to overwrite ${ENV_FILE}." >&2
  exit 1
fi
unlink "${temporary_env}"
trap - EXIT

cat <<MSG
Created ${ENV_FILE}.

Next commands:
  cd "${SCRIPT_DIR}" && ./redeploy.sh
  tailscale funnel --bg 3000
MSG
