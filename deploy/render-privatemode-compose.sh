#!/usr/bin/env bash
# Render a self-contained measured Compose document while preserving the
# reviewed Privatemode manifest byte-for-byte.

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
  PRIVATEMODE_MANIFEST_PATH \
  PRIVATEMODE_API_KEY
do
  [[ -n ${!name:-} ]] || die "$name must be set"
done

require_tool docker
require_tool jq
require_tool sha256sum

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null && pwd)
manifest_path=$(realpath -- "$PRIVATEMODE_MANIFEST_PATH")
[[ -f $manifest_path ]] || die "manifest is not a regular file: $manifest_path"
jq -e . "$manifest_path" >/dev/null || die "manifest is not valid JSON: $manifest_path"

PRIVATEMODE_MANIFEST_PATH=$manifest_path
PRIVATEMODE_MANIFEST_SHA256=$(sha256sum "$manifest_path" | cut -d' ' -f1)
PRIVATEMODE_CREDENTIAL_SHA256=$(
  printf '%s' "$PRIVATEMODE_API_KEY" | sha256sum | cut -d' ' -f1
)
export \
  PRIVATEMODE_MANIFEST_PATH \
  PRIVATEMODE_MANIFEST_SHA256 \
  PRIVATEMODE_CREDENTIAL_SHA256

mkdir -p -- "$(dirname -- "$output")"
tmp=$(mktemp "${output}.tmp.XXXXXX")
trap 'rm -f -- "$tmp"' EXIT

# `docker compose config` keeps file-backed configs as host paths. Replace that
# one path with a JSON string loaded by jq --rawfile, which preserves every
# manifest byte including whitespace and the final newline.
docker compose -f "$script_dir/compose.privatemode.yaml" config --format json \
  | jq --rawfile manifest "$manifest_path" '
      .configs["privatemode-manifest"].content = $manifest
      | del(.configs["privatemode-manifest"].file)
    ' >"$tmp"

rendered_manifest_sha256=$(
  jq -j '.configs["privatemode-manifest"].content' "$tmp" | sha256sum | cut -d' ' -f1
)
[[ $rendered_manifest_sha256 == "$PRIVATEMODE_MANIFEST_SHA256" ]] \
  || die "rendered manifest digest changed: expected $PRIVATEMODE_MANIFEST_SHA256, got $rendered_manifest_sha256"

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

printf '[%s] wrote %s (manifest sha256:%s)\n' \
  "$PROG" "$output" "$PRIVATEMODE_MANIFEST_SHA256" >&2
