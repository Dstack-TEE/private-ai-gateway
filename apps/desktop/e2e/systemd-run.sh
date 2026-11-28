#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: apps/desktop/e2e/run.sh --systemd [--matrix ROW] [--tag TAG | --run RUN_ID | --packages DIR] [--skip-request]

Rows: ubuntu-deb-opencode, ubuntu-script-npm, fedora-rpm-bun,
      arch-pacman-opencode, ubuntu-npm-nvm
EOF
}

tag=""
run_id=""
packages_dir=""
skip_request=false
row=""
while (($#)); do
  case "$1" in
    --tag) tag="${2:?--tag needs a value}"; shift 2 ;;
    --run) run_id="${2:?--run needs a workflow run id}"; shift 2 ;;
    --packages) packages_dir="${2:?--packages needs a directory}"; shift 2 ;;
    --skip-request) skip_request=true; shift ;;
    --matrix) row="${2:?--matrix needs a value}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown systemd option: $1" >&2; usage >&2; exit 2 ;;
  esac
done
case "$row" in
  ""|ubuntu-deb-opencode|ubuntu-script-npm|fedora-rpm-bun|arch-pacman-opencode|ubuntu-npm-nvm) ;;
  *) echo "Unknown matrix row: $row" >&2; exit 2 ;;
esac
if [[ -n "$run_id" || -n "$packages_dir" ]] && [[ "$row" == ubuntu-script-npm || "$row" == ubuntu-npm-nvm ]]; then
  echo "$row requires --tag because the package-only artifact does not contain a published install-script or npm package" >&2
  exit 2
fi
sources=0
[[ -n "$tag" ]] && sources=$((sources + 1))
[[ -n "$run_id" ]] && sources=$((sources + 1))
[[ -n "$packages_dir" ]] && sources=$((sources + 1))
if ((sources > 1)); then
  echo "Pass only one of --tag, --run or --packages" >&2
  exit 2
fi
if [[ "$skip_request" != true && -z "${PAP_E2E_API_KEY:-}" ]]; then
  echo "Set PAP_E2E_API_KEY to a RedPill API key, or pass --skip-request" >&2
  exit 2
fi
command -v docker >/dev/null || { echo "docker is required" >&2; exit 2; }
command -v gh >/dev/null || { echo "gh is required" >&2; exit 2; }
command -v jq >/dev/null || { echo "jq is required" >&2; exit 2; }

e2e_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/pap-e2e-systemd.XXXXXX")"
container="$(basename "$work")"
network=bridge
built_images=()
cleanup() {
  local status=$?
  docker rm --force "$container" >/dev/null 2>&1 || true
  for image in "${built_images[@]}"; do
    docker image rm --force "$image" >/dev/null 2>&1 || true
  done
  if [[ "$status" -ne 0 ]]; then
    evidence_dir="$work.failure"
    mkdir "$evidence_dir"
    for file in status.json doctor.json resumed-status.json backend.log; do
      [[ -f "$work/$file" ]] && cp "$work/$file" "$evidence_dir/$file"
    done
    rm -rf "$work"
    echo "Evidence preserved at $evidence_dir" >&2
  else
    rm -rf "$work"
  fi
  return "$status"
}
trap cleanup EXIT

handle_markers() {
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
}

wait_for_systemd() {
  local state status
  for _ in $(seq 1 60); do
    set +e
    state="$(docker exec "$container" systemctl is-system-running --wait 2>&1)"
    status=$?
    set -e
    case "$state" in
      running|degraded) echo "systemd: $state"; return 0 ;;
    esac
    sleep 1
  done
  echo "systemd did not become ready (status $status): $state" >&2
  docker inspect "$container" --format 'container state: {{.State.Status}} exit={{.State.ExitCode}} error={{.State.Error}}' >&2 || true
  docker logs "$container" 2>&1 | tail -n 40 >&2 || true
  docker exec "$container" systemctl --failed --no-legend >&2 || true
  return 1
}

env -u PAP_E2E_API_KEY node "$e2e_dir/resolve-versions.mjs" >"$work/versions.env"
mkdir "$work/pkg"
repo="${GH_REPO:-Dstack-TEE/private-ai-gateway}"
if [[ -n "$packages_dir" ]]; then
  cp -a "$packages_dir/." "$work/pkg/"
  if [[ -f "$work/pkg/SHA256SUMS" ]]; then
    (cd "$work/pkg" && sha256sum --strict --check SHA256SUMS)
  else
    echo "Package directory must contain SHA256SUMS" >&2
    exit 1
  fi
elif [[ -n "$run_id" ]]; then
  run_json="$(gh run view "$run_id" --repo "$repo" --json status,conclusion,headSha,workflowName,jobs)"
  [[ "$(jq -r .status <<<"$run_json")" == completed ]] || {
    echo "Workflow run $run_id is not completed" >&2
    exit 1
  }
  workflow_name="$(jq -r .workflowName <<<"$run_json")"
  [[ "$workflow_name" == "Desktop Tauri" ]] || { echo "Workflow run $run_id is not Desktop Tauri" >&2; exit 1; }
  if ! jq -e 'any(.jobs[]; .name == "Package (linux-x64)" and .conclusion == "success")' <<<"$run_json" >/dev/null; then
    echo "Workflow run $run_id has no successful Package (linux-x64) job" >&2
    exit 1
  fi
  head_sha="$(jq -r .headSha <<<"$run_json")"
  echo "Testing package workflow run $run_id at $head_sha"
  artifact_json="$(gh api "repos/$repo/actions/runs/$run_id/artifacts?per_page=100")"
  artifact_id="$(jq -r '.artifacts[] | select(.name == "desktop-package-linux-x64") | .id' <<<"$artifact_json" | head -n 1)"
  artifact_digest="$(jq -r '.artifacts[] | select(.name == "desktop-package-linux-x64") | .digest' <<<"$artifact_json" | head -n 1)"
  [[ -n "$artifact_id" && "$artifact_id" != null ]] || { echo "Linux package artifact is missing from run $run_id" >&2; exit 1; }
  if [[ -n "$artifact_digest" && "$artifact_digest" != null ]]; then
    gh api "repos/$repo/actions/artifacts/$artifact_id/zip" >"$work/package-artifact.zip"
    expected_digest="${artifact_digest#sha256:}"
    actual_digest="$(sha256sum "$work/package-artifact.zip" | awk '{print $1}')"
    [[ "$actual_digest" == "$expected_digest" ]] || { echo "Artifact digest mismatch" >&2; exit 1; }
    unzip -q "$work/package-artifact.zip" -d "$work/pkg"
  else
    gh run download "$run_id" --repo "$repo" --name desktop-package-linux-x64 --dir "$work/pkg"
  fi
  if [[ -f "$work/pkg/SHA256SUMS" ]]; then
    (cd "$work/pkg" && sha256sum --strict --check SHA256SUMS)
  elif [[ -z "$artifact_digest" || "$artifact_digest" == null ]]; then
    echo "Package artifact has neither SHA256SUMS nor a GitHub digest" >&2
    exit 1
  else
    echo "Verified package artifact with GitHub's $artifact_digest digest"
  fi
else
  if [[ -z "$tag" ]]; then
    tag="$(gh release list --repo "$repo" --exclude-drafts --limit 100 --json tagName --jq '[.[].tagName | select(startswith("desktop-v"))][0] // empty')"
  fi
  [[ -n "$tag" ]] || { echo "No published desktop-v* release; pass --tag" >&2; exit 1; }
  gh release download "$tag" --repo "$repo" --dir "$work/pkg" \
    --pattern "private-ai-proxy-cli-${tag#desktop-v}-linux-x64.deb" \
    --pattern "private-ai-proxy-cli-${tag#desktop-v}-linux-x64.rpm" \
    --pattern "private-ai-proxy-cli-${tag#desktop-v}-linux-x64.pkg.tar.zst" \
    --pattern SHA256SUMS \
    --pattern "private-ai-proxy-cli-${tag#desktop-v}-linux-x64.tar.gz"
  (cd "$work/pkg" && sha256sum --ignore-missing --strict --check SHA256SUMS)
fi

rows=(ubuntu-deb-opencode ubuntu-script-npm fedora-rpm-bun arch-pacman-opencode ubuntu-npm-nvm)
declare -A image_files=(
  [ubuntu-deb-opencode]=ubuntu-systemd.Dockerfile
  [ubuntu-script-npm]=ubuntu-systemd.Dockerfile
  [ubuntu-npm-nvm]=ubuntu-systemd.Dockerfile
  [fedora-rpm-bun]=fedora-systemd.Dockerfile
  [arch-pacman-opencode]=arch-systemd.Dockerfile
)
declare -A images=()
for selected in "${rows[@]}"; do
  [[ -z "$row" || "$row" == "$selected" ]] || continue
  image="pap-e2e-systemd-${selected}"
  if [[ -z "${images[$image]:-}" ]]; then
    docker build --tag "$image" --file "$e2e_dir/container/${image_files[$selected]}" "$e2e_dir/container" >/dev/null
    images[$image]=built
    built_images+=("$image")
  fi
  echo "=== systemd matrix row: $selected ($image) ==="
  docker run --detach --privileged --cap-add=AUDIT_CONTROL --cap-add=AUDIT_WRITE --cgroupns=private \
    --name "$container" --network "$network" \
    --tmpfs /run --tmpfs /run/lock --tmpfs /tmp:rw,size=1g \
    --volume "$e2e_dir:/e2e:ro" --volume "$work/pkg:/pkg:ro" \
    --volume "$work:/work" \
    --env-file "$work/versions.env" --env PAP_E2E_MATRIX="$selected" \
    --env PAP_E2E_RELEASE_TAG="${tag:-build}" --env PAP_E2E_AGENTS=opencode \
    --env PAP_E2E_SKIP_REQUEST="$skip_request" \
    --env PAP_E2E_OPENCODE_V2=1 --env AGENT_PATH=/home/tester/.local/bin:/home/tester/.npm-global/bin:/home/tester/.opencode/bin:/home/tester/.bun/bin \
    "$image" >/dev/null
  wait_for_systemd
  docker exec "$container" /e2e/container/prepare-systemd.sh
  docker exec "$container" bash -euc '
    for pid in $(pgrep -u tester); do
      command_line=$(ps -p "$pid" -o args=)
      case "$command_line" in
        *"systemd --user"*|*dbus*|*private-ai-proxy-service*) ;;
        *) kill -KILL "$pid" 2>/dev/null || [[ ! -e "/proc/$pid" ]] ;;
      esac
    done
  '
  set +e
  if [[ "$skip_request" == true ]]; then
    docker exec "$container" /e2e/container/systemd-e2e.sh prepare | handle_markers
    test_status=${PIPESTATUS[0]}
  else
    printf '%s' "$PAP_E2E_API_KEY" | docker exec --interactive "$container" /e2e/container/systemd-e2e.sh prepare | handle_markers
    test_status=${PIPESTATUS[1]}
  fi
  set -e
  test "$test_status" -eq 0
  if [[ "$skip_request" == true ]]; then
    echo "[$selected] no-request smoke stopped before profile verification"
    docker rm --force "$container" >/dev/null
    continue
  fi
  echo "[$selected] restarting the systemd container"
  docker restart "$container" >/dev/null
  wait_for_systemd
  docker exec "$container" /e2e/container/systemd-e2e.sh resume
  docker rm --force "$container" >/dev/null
done
