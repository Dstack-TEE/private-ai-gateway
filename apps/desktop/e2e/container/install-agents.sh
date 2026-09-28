#!/usr/bin/env bash
# Runs as the test user: installs every agent with its official installer at
# the version pinned in versions.env, then runs it once. Its first run creates
# the configuration folder that Private AI Proxy detects the agent by.
set -euo pipefail

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

# First runs. Each creates the folder detection looks for and exits 0
# without a provider.
claude --version
codex --version
opencode --version
pi --list-models >/dev/null
omp config list >/dev/null
openclaw setup --baseline >/dev/null
hermes --version
