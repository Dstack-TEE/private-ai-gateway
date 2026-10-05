#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: apps/desktop/e2e/run.sh [--tag desktop-vX.Y.Z | --deb PATH]
                               [--agents ID,...] [--opencode-v2]

Installs the Linux x64 desktop package in a fresh ubuntu:24.04 container,
connects every supported coding agent to a live RedPill profile, and checks
replies, usage records, fail-closed behavior, restoration and token revocation.

  --tag TAG      Download this release's package and SHA256SUMS. Default: the
                 newest published desktop-v* release, beta or stable.
  --deb PATH     Test a local package; PATH's directory must hold its SHA256SUMS.
  --agents IDS   Install and test only these comma-separated agent ids.
  --opencode-v2  Install OpenCode 2 (OPENCODE_V2_VERSION) instead of OpenCode 1.

Environment:
  PAP_E2E_API_KEY  RedPill API key (required). It reaches only the stdin of
                   `pap profiles add` inside the container.
  GH_REPO          Repository for --tag. Default: Dstack-TEE/private-ai-gateway

Requirements: docker, sha256sum, and gh unless --deb is given.
EOF
}

tag=""
deb=""
agents=""
opencode_v2=false
while (($#)); do
  case "$1" in
    --tag) tag="${2:?--tag needs a value}"; shift 2 ;;
    --deb) deb="${2:?--deb needs a value}"; shift 2 ;;
    --agents) agents="${2:?--agents needs a value}"; shift 2 ;;
    --opencode-v2) opencode_v2=true; shift ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
if [[ -n "$tag" && -n "$deb" ]]; then
  echo "Pass --tag or --deb, not both" >&2
  exit 2
fi
if [[ -n "$agents" && ! "$agents" =~ ^[a-z-]+(,[a-z-]+)*$ ]]; then
  echo "--agents takes comma-separated agent ids" >&2
  exit 2
fi
if [[ -z "${PAP_E2E_API_KEY:-}" ]]; then
  echo "Set PAP_E2E_API_KEY to a RedPill API key" >&2
  exit 2
fi

e2e_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/pap-e2e.XXXXXX")"
container="$(basename "$work")"
network=bridge
versions=(--env-file "$e2e_dir/versions.env")
if [[ "$opencode_v2" == true ]]; then
  versions+=(--env PAP_E2E_OPENCODE_V2=1)
fi
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
      --jq '[.[].tagName | select(startswith("desktop-v"))][0] // empty')"
    if [[ -z "$tag" ]]; then
      echo "No published desktop-v* release in $repo; pass --tag or --deb" >&2
      exit 1
    fi
  fi
  gh release download "$tag" --repo "$repo" --dir "$work/pkg" \
    --pattern "private-ai-proxy-${tag#desktop-v}-linux-x64.deb" --pattern SHA256SUMS
fi
(cd "$work/pkg" && sha256sum --check --ignore-missing --strict SHA256SUMS)

# AGENT_PATH holds the installer-writable directories the agents run from.
# Only install-agents.sh and the agents themselves search them; processes
# that see the key run from absolute paths with the system PATH.
docker run --detach --init --name "$container" --network "$network" "${versions[@]}" \
  --env PAP_E2E_AGENTS="$agents" --env DISABLE_AUTOUPDATER=1 --env OPENCODE_DISABLE_AUTOUPDATE=1 \
  --env DSH_TELEMETRY_DISABLED=1 \
  --env AGENT_PATH=/home/tester/.local/bin:/home/tester/.npm-global/bin:/home/tester/.opencode/bin \
  --volume "$e2e_dir:/e2e:ro" --volume "$work/pkg:/pkg:ro" ubuntu:24.04 sleep infinity >/dev/null
docker exec "$container" /e2e/container/prepare.sh

as_tester=(docker exec --user tester --workdir /home/tester --env PATH=/usr/local/bin:/usr/bin:/bin)
"${as_tester[@]}" "$container" /e2e/container/install-agents.sh
"${as_tester[@]}" "$container" bash -c \
  'cp -r /e2e ~/e2e && npm ci --prefix ~/e2e --ignore-scripts --no-audit --no-fund --silent'
# Nothing an installer started may still run when the key arrives.
docker exec "$container" bash -c 'pkill --signal KILL --uid tester
  timeout 10 bash -c "while pgrep --uid tester >/dev/null; do sleep 0.2; done" ||
    { pgrep --list-full --uid tester >&2; echo "Test user processes survived" >&2; exit 1; }'
# The test asks for the outage it checks by printing a marker line; it
# confirms the service is unreachable, and later reachable again, itself.
printf '%s' "$PAP_E2E_API_KEY" |
  "${as_tester[@]}" --interactive "$container" /usr/local/bin/node --test-reporter=spec e2e/agents.test.mjs |
  while IFS= read -r line; do
    case "$line" in
      "::pap-e2e network down")
        echo "Disconnecting the container from $network"
        docker network disconnect "$network" "$container"
        ;;
      "::pap-e2e network up")
        echo "Reconnecting the container to $network"
        docker network connect "$network" "$container"
        ;;
      *) printf '%s\n' "$line" ;;
    esac
  done
