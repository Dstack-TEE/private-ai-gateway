#!/usr/bin/env bash
# Runs as the test user: installs every agent with its official installer at
# the version pinned in versions.env, then runs it once. Its first run creates
# the configuration folder that Private AI Proxy detects the agent by.
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

from_script claude-code https://claude.ai/install.sh "$CLAUDE_CODE_VERSION"
logged codex npm install --global "@openai/codex@$CODEX_VERSION"
from_script opencode https://opencode.ai/install --version "$OPENCODE_VERSION" --no-modify-path
logged pi npm install --global --ignore-scripts "@earendil-works/pi-coding-agent@$PI_VERSION"
from_script oh-my-pi https://omp.sh/install --binary --ref "v$OH_MY_PI_VERSION"
from_script openclaw https://openclaw.ai/install.sh --no-onboard --no-prompt
from_script hermes "https://raw.githubusercontent.com/NousResearch/hermes-agent/$HERMES_VERSION/scripts/install.sh" \
  --branch "$HERMES_VERSION" --non-interactive --skip-browser

# Fails when an installer ignored its version pin.
pinned() {
  local expected="$1" reported
  shift
  reported="$("$@" 2>&1)"
  if ! grep -Fqw -- "$expected" <<<"$reported"; then
    printf '%s reports %s, not the pinned %s\n' "$1" "${reported%%$'\n'*}" "$expected" >&2
    return 1
  fi
  echo "$1 $expected"
}

# The first --version runs of Claude Code, Codex and OpenCode, and the other
# commands below, create the folders detection looks for; all exit 0 without
# a provider.
pinned "$CLAUDE_CODE_VERSION" claude --version
pinned "$CODEX_VERSION" codex --version
pinned "$OPENCODE_VERSION" opencode --version
pinned "$PI_VERSION" pi --version
pinned "$OH_MY_PI_VERSION" omp --version
pinned "$OPENCLAW_VERSION" openclaw --version
pinned "${HERMES_VERSION#v}" hermes --version
pi --list-models >/dev/null
omp config list >/dev/null
openclaw setup --baseline >/dev/null
