#!/usr/bin/env bash
set -euo pipefail

row="${PAP_E2E_MATRIX:?PAP_E2E_MATRIX is required}"
phase="${1:-all}"
case "$phase" in prepare|resume|all) ;; *) echo "Unknown phase: $phase" >&2; exit 2 ;; esac
api_key=""
skip_request="${PAP_E2E_SKIP_REQUEST:-false}"
if [[ "$phase" == prepare || "$phase" == all ]] && [[ "$skip_request" != true ]]; then
  api_key="$(cat)"
fi
uid="$(id -u tester)"
case "$row" in
  ubuntu-deb-opencode|fedora-rpm-bun|arch-pacman-opencode) pap=/usr/bin/pap ;;
  ubuntu-script-npm) pap=/home/tester/.local/bin/pap ;;
  ubuntu-npm-nvm) pap="$(find /home/tester/.nvm/versions/node -path '*/bin/pap' -print -quit)" ;;
esac
test -x "$pap"
agent_path=/home/tester/.local/bin:/home/tester/.npm-global/bin:/home/tester/.opencode/bin:/home/tester/.bun/bin:/usr/local/bin:/usr/bin:/bin
agent_dir=/home/tester/.opencode/bin
expected_opencode_version="$OPENCODE_V2_VERSION"
case "$row" in
  fedora-rpm-bun|ubuntu-npm-nvm) expected_opencode_version="$OPENCODE_VERSION" ;;
esac
if [[ "$row" == ubuntu-npm-nvm ]]; then
  agent_path="$(dirname "$pap"):$agent_path"
  agent_dir="$(dirname "$pap")"
fi
unit=private-ai-proxy.service
snapshot_dir=/home/tester/.pap-e2e-config-before
opencode_config=/home/tester/.config/opencode/opencode.json
opencode_snapshot="$snapshot_dir/opencode.json"

as_tester() {
  local command
  printf -v command '%q ' "$@"
  runuser -l tester -c "export HOME=/home/tester PATH='$agent_path' PAP_E2E_PAP='$pap' XDG_RUNTIME_DIR=/run/user/$uid; $command"
}
userctl() { systemctl --user --machine="tester@.host" "$@"; }
check() { as_tester "$@"; }
save_evidence() {
  local name="$1" text="$2"
  printf '%s\n' "$text" >"/work/$name"
  if [[ "$skip_request" != true && -n "$api_key" ]] && grep -Fq "$api_key" "/work/$name"; then
    echo "API key appeared in $name" >&2
    exit 1
  fi
}

if [[ "$phase" == prepare || "$phase" == all ]]; then
  echo "[$row] install"
  for _ in $(seq 1 60); do
    if userctl show-environment >/dev/null 2>&1; then break; fi
    sleep 1
  done
  userctl show-environment >/dev/null
  check "$pap" --version
  check "$pap" doctor
  if [[ "$skip_request" == true ]]; then
    echo "SKIPPED supervision assertions: a pre-key smoke cannot verify pap start without a real profile key"
    exit 0
  fi
  echo "[$row] profile, connect and real request"
  as_tester opencode --version | grep -Eq "v?${expected_opencode_version%%.*}\."
  mkdir -p /home/tester/e2e
  cp -a /e2e/. /home/tester/e2e/
  chown -R tester:tester /home/tester/e2e
  rm -rf "$snapshot_dir"
  mkdir -p "$snapshot_dir"
  if [[ -e "$opencode_config" ]]; then
    cp "$opencode_config" "$opencode_snapshot"
  fi
  if [[ "$skip_request" == true ]]; then
    echo "SKIPPED request: --skip-request was supplied"
  else
    check npm ci --prefix /home/tester/e2e --ignore-scripts --no-audit --no-fund --silent
  printf '%s' "$api_key" | runuser -l tester -c "env PAP_E2E_KEEP_SERVICE=1 PAP_E2E_PAP='$pap' PAP_E2E_AGENTS=opencode HOME=/home/tester PATH='$agent_path' XDG_RUNTIME_DIR=/run/user/$uid node --test-reporter=spec /home/tester/e2e/agents.test.mjs"
  fi
  userctl is-enabled "$unit"
  userctl is-active "$unit"
  main_pid="$(userctl show "$unit" --property=MainPID --value)"
  test "$main_pid" -gt 0
  service_environment="$(tr '\0' '\n' < "/proc/$main_pid/environ")"
  if grep -Fq "$agent_dir" <<<"$service_environment"; then
    echo "service environment unexpectedly contains $agent_dir" >&2
    exit 1
  fi
  if ! grep -q private-ai-proxy <(as_tester ps -o pid=,args= -p "$main_pid"); then
    echo "MainPID $main_pid is not the Private AI Proxy backend" >&2
    exit 1
  fi
fi

[[ "$phase" != prepare ]] || exit 0

for _ in $(seq 1 60); do
  if userctl is-active "$unit" >/dev/null 2>&1; then break; fi
  sleep 1
done
userctl is-active "$unit"

echo "[$row] status and doctor"
status_json="$(check "$pap" --json status)"
doctor_json="$(check "$pap" --json doctor)"
if [[ "$skip_request" != true ]]; then
  for _ in $(seq 1 90); do
    if jq -e '.gateway.protection.phase == "protected" and .gateway.status == "verified"' <<<"$status_json" >/dev/null 2>&1; then
      break
    fi
    sleep 1
    status_json="$(check "$pap" --json status)"
  done
fi
save_evidence status.json "$status_json"
save_evidence doctor.json "$doctor_json"
save_evidence backend.log "$(journalctl _UID="$uid" -u "$unit" --no-pager -n 100 2>&1 || true)"
printf '%s\n' "$status_json" >/tmp/pap-systemd-status.json
printf '%s\n' "$doctor_json" >/tmp/pap-systemd-doctor.json
expected_installation=deb
case "$row" in
  fedora-rpm-bun) expected_installation=rpm ;;
  arch-pacman-opencode) expected_installation=pacman ;;
  ubuntu-script-npm) expected_installation=installScript ;;
  ubuntu-npm-nvm) expected_installation=npm ;;
esac
if [[ "$skip_request" == true ]]; then
  jq -e '.backend.processId > 0 and .loginService.active == true' <<<"$status_json" >/dev/null || { echo "status did not report an active backend" >&2; printf '%s\n' "$status_json" >&2; exit 1; }
else
  if ! jq -e '.gateway.protection.phase == "protected" and .gateway.status == "verified"' <<<"$status_json" >/dev/null; then
    echo "status did not report verified protection within 90 seconds" >&2
    printf '%s\n' "$status_json" >&2
    exit 1
  fi
fi
expected_command_path=/usr/bin/private-ai-proxy
case "$row" in
  ubuntu-script-npm) expected_command_path=/home/tester/.local/bin/private-ai-proxy ;;
  ubuntu-npm-nvm) expected_command_path="$(dirname "$pap")/private-ai-proxy" ;;
esac
jq -e --arg expected "$expected_command_path" '.cli.installed == true and .cli.commandPath == $expected' <<<"$doctor_json" >/dev/null || { echo "doctor did not identify the expected command installation path $expected_command_path" >&2; printf '%s\n' "$doctor_json" >&2; exit 1; }
echo "Doctor does not expose a package-manager installation-kind field; verified cli.commandPath instead."
if jq -e '.settings.warnings | tostring | test("environment mismatch|path"; "i")' <<<"$doctor_json" >/dev/null; then
  echo "doctor reported an environment mismatch" >&2
  exit 1
fi

echo "[$row] relogin and resume"
loginctl terminate-user tester
for _ in $(seq 1 60); do
  if userctl is-active "$unit" >/dev/null 2>&1; then break; fi
  sleep 1
done
userctl is-active "$unit"
resumed_status=""
for _ in $(seq 1 90); do
  set +e
  resumed_status="$(check "$pap" --json status 2>&1)"
  status_code=$?
  set -e
  if [[ "$status_code" -eq 0 ]] && jq -e '.gateway.protection.phase == "protected" and .gateway.status == "verified" and .gateway.sessionActive == true' <<<"$resumed_status" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
save_evidence resumed-status.json "$resumed_status"
if [[ "$skip_request" == true ]]; then
  jq -e '.backend.processId > 0 and .loginService.active == true' <<<"$resumed_status" >/dev/null
else
  if ! jq -e '.gateway.protection.phase == "protected" and .gateway.status == "verified" and .gateway.sessionActive == true' <<<"$resumed_status" >/dev/null; then
    echo "protection did not resume within 90 seconds after restart" >&2
    printf '%s\n' "$resumed_status" >&2
    exit 1
  fi
  as_tester opencode run 'Reply with exactly: PAP-OK' | grep -Fq PAP-OK
fi

echo "[$row] update"
echo "SKIPPED update: no older package was supplied for this run"

echo "[$row] uninstall"
case "$row" in
  ubuntu-script-npm|ubuntu-npm-nvm)
    check "$pap" --yes service disable --for-uninstall
    check "$pap" --yes service stop
    check "$pap" --yes stop --offline
    ;;
esac
# The native package managers run the per-user cleanup in the removal hook.
case "$row" in
  ubuntu-deb-opencode) apt-get remove -y private-ai-proxy-cli >/dev/null ;;
  fedora-rpm-bun) dnf remove -y private-ai-proxy-cli >/dev/null ;;
  arch-pacman-opencode) pacman -R --noconfirm private-ai-proxy-cli >/dev/null ;;
  ubuntu-script-npm) rm -f /home/tester/.local/bin/{pap,aci,private-ai-proxy}; rm -rf /home/tester/.local/share/private-ai-proxy-custom ;;
  ubuntu-npm-nvm) check npm uninstall --global private-ai-proxy >/dev/null ;;
esac
active_state="$(userctl show "$unit" --property=ActiveState --value)"
if [[ "$active_state" != inactive ]]; then
  echo "Expected an inactive service after uninstall, got $active_state" >&2
  exit 1
fi
for unit_path in "/usr/lib/systemd/user/$unit" "/home/tester/.config/systemd/user/$unit" \
  "/home/tester/.config/systemd/user/default.target.wants/$unit"; do
  if [[ -e "$unit_path" || -L "$unit_path" ]]; then
    echo "Uninstall left unit file or link $unit_path" >&2
    exit 1
  fi
done
for bin_dir in /home/tester/.local/bin "${pap%/*}"; do
  for link in pap aci private-ai-proxy; do
    if [[ -e "$bin_dir/$link" || -L "$bin_dir/$link" ]]; then
      echo "Uninstall left command or link $bin_dir/$link" >&2
      exit 1
    fi
  done
done
test ! -e /home/tester/.local/share/private-ai-proxy-custom
if [[ -d /home/tester/.nvm/versions/node ]] && find /home/tester/.nvm/versions/node -path '*/bin/pap' -print -quit | grep -q .; then
  echo "npm installation left a pap link" >&2
  exit 1
fi
for dotfile in .bashrc .profile .bash_profile; do
  if [[ -e "$snapshot_dir/../.pap-e2e-dotfiles-before/$dotfile.missing" ]]; then
    test ! -e "/home/tester/$dotfile"
  else
    cmp "$snapshot_dir/../.pap-e2e-dotfiles-before/$dotfile" "/home/tester/$dotfile"
  fi
done
assert_opencode_restored() {
  local snapshot="$1" config="$2" expected='{}' actual='{}'
  local normalize='
    if length == 1 and (.[0] | type) == "object" then
      .[0] | del(.autoupdate, .["$schema"])
    else
      error("Expected one OpenCode configuration object")
    end
  '
  if [[ -e "$snapshot" ]]; then
    if [[ ! -e "$config" ]]; then
      echo "Uninstall removed the original OpenCode config" >&2
      return 1
    fi
    expected="$(jq -csSe "$normalize" "$snapshot")" || return 1
  fi
  if [[ -e "$config" || -L "$config" ]]; then
    actual="$(jq -csSe "$normalize" "$config")" || return 1
  fi
  if [[ "$actual" != "$expected" ]]; then
    echo "Uninstall did not restore the OpenCode config" >&2
    return 1
  fi
}
assert_opencode_restored "$opencode_snapshot" "$opencode_config"
rm -rf "$snapshot_dir"
rm -rf /home/tester/.pap-e2e-dotfiles-before
echo "[$row] PASS"
