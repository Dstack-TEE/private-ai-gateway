#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: apps/desktop/e2e/run.sh [--tag desktop-vX.Y.Z | --deb PATH]

Installs the Linux x64 desktop package in a fresh ubuntu:24.04 container,
connects every supported coding agent to a live RedPill profile, and checks
replies, usage records, fail-closed behavior, restoration and token revocation.

  --tag TAG   Download this release's package and SHA256SUMS. Default: the
              newest published desktop-v* release, beta or stable.
  --deb PATH  Test a local package; PATH's directory must hold its SHA256SUMS.

Environment:
  PAP_E2E_API_KEY  RedPill API key (required). It reaches only the stdin of
                   `pap profiles add` inside the container.
  GH_REPO          Repository for --tag. Default: Dstack-TEE/private-ai-gateway

Requirements: docker, sha256sum, and gh unless --deb is given.
EOF
}

tag=""
deb=""
while (($#)); do
  case "$1" in
    --tag) tag="${2:?--tag needs a value}"; shift 2 ;;
    --deb) deb="${2:?--deb needs a value}"; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
if [[ -n "$tag" && -n "$deb" ]]; then
  echo "Pass --tag or --deb, not both" >&2
  exit 2
fi
if [[ -z "${PAP_E2E_API_KEY:-}" ]]; then
  echo "Set PAP_E2E_API_KEY to a RedPill API key" >&2
  exit 2
fi

e2e_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/pap-e2e.XXXXXX")"
container="$(basename "$work")"
cleanup() {
  docker rm --force "$container" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

mkdir "$work/pkg"
if [[ -n "$deb" ]]; then
  cp "$deb" "$(dirname "$deb")/SHA256SUMS" "$work/pkg/"
else
  repo="${GH_REPO:-Dstack-TEE/private-ai-gateway}"
  if [[ -z "$tag" ]]; then
    tag="$(gh release list --repo "$repo" --exclude-drafts --limit 100 --json tagName \
      --jq '[.[].tagName | select(startswith("desktop-v"))][0]')"
  fi
  gh release download "$tag" --repo "$repo" --dir "$work/pkg" \
    --pattern "private-ai-proxy-${tag#desktop-v}-linux-x64.deb" --pattern SHA256SUMS
fi
(cd "$work/pkg" && sha256sum --check --ignore-missing --strict SHA256SUMS)

docker run --detach --init --name "$container" --env-file "$e2e_dir/versions.env" \
  --volume "$e2e_dir:/e2e:ro" --volume "$work/pkg:/pkg:ro" ubuntu:24.04 sleep infinity >/dev/null
docker exec "$container" /e2e/container/prepare.sh

as_tester=(docker exec --user tester --workdir /home/tester
  --env "PATH=/home/tester/.local/bin:/home/tester/.npm-global/bin:/home/tester/.opencode/bin:/usr/local/bin:/usr/bin:/bin")
"${as_tester[@]}" "$container" /e2e/container/install-agents.sh
"${as_tester[@]}" "$container" bash -c \
  'cp -r /e2e ~/e2e && npm ci --prefix ~/e2e --ignore-scripts --no-audit --no-fund --silent'
printf '%s' "$PAP_E2E_API_KEY" |
  "${as_tester[@]}" --interactive "$container" node --test-reporter=spec e2e/agents.test.mjs
