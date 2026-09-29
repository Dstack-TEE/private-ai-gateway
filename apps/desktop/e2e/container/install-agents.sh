#!/usr/bin/env bash
# Runs as the test user: installs each agent under test (PAP_E2E_AGENTS, or
# every agent when empty) with its official installer at the version pinned in
# versions.env, then runs it once. Its first run creates the configuration
# folder that Private AI Proxy detects the agent by.
set -euo pipefail
export PATH="$AGENT_PATH:$PATH"

logs="$HOME/install-logs"
mkdir -p "$logs"
npm config set prefix "$HOME/.npm-global"

# Keeps an installer's output in a log and shows its end only on failure.
logged() {
  local name="$1"
  shift
  if ! "$@" >"$logs/$name.log" 2>&1; then
    tail -n 40 "$logs/$name.log" >&2
    echo "Installing $name failed" >&2
    return 1
  fi
}

# Downloads an official install script, then runs it with the arguments.
from_script() {
  local name="$1" url="$2"
  shift 2
  curl -fsSL --proto '=https' --tlsv1.2 "$url" -o "$logs/$name-install"
  chmod +x "$logs/$name-install"
  logged "$name" "$logs/$name-install" "$@"
}

# Fails when an installer ignored its version pin. OpenCode 2 prints a v prefix.
pinned() {
  local expected="$1" reported
  shift
  reported="$("$@" 2>&1)"
  if ! grep -Eqw -- "v?${expected//./\\.}" <<<"$reported"; then
    printf '%s reports %s, not the pinned %s\n' "$1" "${reported%%$'\n'*}" "$expected" >&2
    return 1
  fi
  echo "$1 $expected"
}

# The first --version runs of Claude Code, Codex and OpenCode, and the other
# commands below, create the folders detection looks for; all exit 0 without
# a provider.
for agent in claude-code codex dsh opencode pi oh-my-pi openclaw hermes; do
  [[ -z "${PAP_E2E_AGENTS:-}" || ",$PAP_E2E_AGENTS," == *",$agent,"* ]] || continue
  case "$agent" in
    claude-code)
      from_script claude-code https://claude.ai/install.sh "$CLAUDE_CODE_VERSION"
      pinned "$CLAUDE_CODE_VERSION" claude --version
      ;;
    codex)
      logged codex npm install --global "@openai/codex@$CODEX_VERSION"
      pinned "$CODEX_VERSION" codex --version
      ;;
    dsh)
      logged dsh npm install --global --ignore-scripts "@deepseek-ai/dsh@$DSH_VERSION"
      pinned "$DSH_VERSION" dsh --version
      dsh headless --help >/dev/null
      ;;
    opencode)
      # OpenCode 2 has its own installer; both install the opencode command.
      [[ "${PAP_E2E_OPENCODE_V2:-}" != 1 ]] || OPENCODE_VERSION="$OPENCODE_V2_VERSION"
      installer=https://opencode.ai/install
      [[ "$OPENCODE_VERSION" == 2.* ]] && installer=https://opencode.ai/v2/install
      from_script opencode "$installer" --version "$OPENCODE_VERSION" --no-modify-path
      pinned "$OPENCODE_VERSION" opencode --version
      ;;
    pi)
      logged pi npm install --global --ignore-scripts "@earendil-works/pi-coding-agent@$PI_VERSION"
      pinned "$PI_VERSION" pi --version
      pi --list-models >/dev/null
      ;;
    oh-my-pi)
      from_script oh-my-pi https://omp.sh/install --binary --ref "v$OH_MY_PI_VERSION"
      pinned "$OH_MY_PI_VERSION" omp --version
      omp config list >/dev/null
      ;;
    openclaw)
      from_script openclaw https://openclaw.ai/install.sh --no-onboard --no-prompt
      pinned "$OPENCLAW_VERSION" openclaw --version
      openclaw setup --baseline >/dev/null
      ;;
    hermes)
      from_script hermes "https://raw.githubusercontent.com/NousResearch/hermes-agent/$HERMES_VERSION/scripts/install.sh" \
        --branch "$HERMES_VERSION" --non-interactive --skip-browser
      pinned "${HERMES_VERSION#v}" hermes --version
      ;;
  esac
done
