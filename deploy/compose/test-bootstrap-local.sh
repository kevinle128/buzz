#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_DIR}"' EXIT

cp \
  "${SCRIPT_DIR}/bootstrap-local.sh" \
  "${SCRIPT_DIR}/run.sh" \
  "${SCRIPT_DIR}/.env.example" \
  "${TEST_DIR}/"
chmod +x "${TEST_DIR}/bootstrap-local.sh" "${TEST_DIR}/run.sh"

expect_failure() {
  if "$@"; then
    echo "Expected command to fail: $*" >&2
    exit 1
  fi
}

owner_pubkey="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
"${TEST_DIR}/run.sh" bootstrap-local relay-node.example.ts.net "${owner_pubkey}" >/dev/null

env_file="${TEST_DIR}/.env"
[[ -f "${env_file}" ]]
# Try GNU stat first: GNU `stat -f` reports filesystem status and succeeds,
# so it cannot be the probe. BSD stat rejects -c and falls through.
[[ "$(stat -c '%a' "${env_file}" 2>/dev/null || stat -f '%Lp' "${env_file}")" == "600" ]]
grep -Fxq 'BUZZ_IMAGE=buzz-local:current' "${env_file}"
grep -Fxq 'RELAY_URL=wss://relay-node.example.ts.net' "${env_file}"
grep -Fxq 'BUZZ_MEDIA_BASE_URL=https://relay-node.example.ts.net/media' "${env_file}"
grep -Fxq 'BUZZ_REQUIRE_MEDIA_GET_AUTH=true' "${env_file}"
grep -Fxq "RELAY_OWNER_PUBKEY=${owner_pubkey}" "${env_file}"
if grep -Eq '^[[:space:]]*[A-Za-z_][A-Za-z0-9_]*=.*CHANGE_ME' "${env_file}"; then
  echo "Generated environment contains a CHANGE_ME value." >&2
  exit 1
fi
grep -Eq '^BUZZ_RELAY_PRIVATE_KEY=[0-9a-f]{64}$' "${env_file}"
grep -Eq '^POSTGRES_PASSWORD=[0-9a-f]{64}$' "${env_file}"
grep -Eq '^BUZZ_S3_ACCESS_KEY=buzz[0-9a-f]{24}$' "${env_file}"

checksum_before="$(shasum -a 256 "${env_file}")"
expect_failure "${TEST_DIR}/bootstrap-local.sh" relay-node.example.ts.net "${owner_pubkey}" >/dev/null 2>&1
[[ "$(shasum -a 256 "${env_file}")" == "${checksum_before}" ]]

rm "${env_file}"
expect_failure "${TEST_DIR}/bootstrap-local.sh" 'https://bad.example' "${owner_pubkey}" >/dev/null 2>&1
expect_failure "${TEST_DIR}/bootstrap-local.sh" 'bad..example' "${owner_pubkey}" >/dev/null 2>&1
expect_failure "${TEST_DIR}/bootstrap-local.sh" '.bad.example' "${owner_pubkey}" >/dev/null 2>&1
expect_failure "${TEST_DIR}/bootstrap-local.sh" relay-node.example.ts.net invalid >/dev/null 2>&1

echo "bootstrap-local tests passed"
