#!/usr/bin/env bash
# Render a self-contained measured Compose document.

set -euo pipefail

PROG=render-privatemode-compose.sh
die() { printf '[%s] error: %s\n' "$PROG" "$*" >&2; exit 1; }
require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "required tool not found in PATH: $1"
}

[[ $# -eq 1 ]] || die "usage: $PROG OUTPUT.json"
output=$1
[[ -n $output ]] || die "output path must not be empty"

for name in \
  PRIVATE_AI_GATEWAY_REPO_COMMIT \
  PRIVATE_AI_GATEWAY_ADMIN_TOKEN_SHA256 \
  PRIVATE_AI_GATEWAY_INFERENCE_TOKEN_SHA256 \
  PRIVATEMODE_API_KEY
do
  [[ -n ${!name:-} ]] || die "$name must be set"
done

require_tool docker
require_tool sha256sum

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null && pwd)
PRIVATEMODE_CREDENTIAL_SHA256=$(
  printf '%s' "$PRIVATEMODE_API_KEY" | sha256sum | cut -d' ' -f1
)
export PRIVATEMODE_CREDENTIAL_SHA256

mkdir -p -- "$(dirname -- "$output")"
tmp=$(mktemp "${output}.tmp.XXXXXX")
trap 'rm -f -- "$tmp"' EXIT

docker compose -f "$script_dir/compose.privatemode.yaml" config --format json \
  >"$tmp"

for secret_name in \
  PRIVATE_AI_GATEWAY_ADMIN_TOKEN \
  PRIVATE_AI_GATEWAY_INFERENCE_TOKEN \
  PRIVATEMODE_API_KEY
do
  secret_value=${!secret_name:-}
  if [[ -n $secret_value ]] && grep -Fq -- "$secret_value" "$tmp"; then
    die "$secret_name was embedded in the rendered Compose"
  fi
done

docker compose -f "$tmp" config --quiet
mv -- "$tmp" "$output"
trap - EXIT

printf '[%s] wrote %s\n' "$PROG" "$output" >&2
