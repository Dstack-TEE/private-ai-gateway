# Live end-to-end test

Tests a published Linux x64 desktop package against the live RedPill service
with every supported coding agent: Claude Code, Codex, OpenCode, Pi, Oh My Pi,
OpenClaw and Hermes. `Desktop live E2E` (`.github/workflows/desktop-e2e.yml`)
runs it nightly and before a stable release
([Live end-to-end test](../docs/distribution.md#live-end-to-end-test)).

`run.sh` verifies the package against the release's `SHA256SUMS` and starts a
fresh `ubuntu:24.04` container. There it installs the package, Node.js and
each agent with the agent's official installer as an unprivileged user. It
checks that every agent reports its pinned version, and runs each agent once
so that it creates the configuration folder Private AI Proxy detects. Once no
process of that user is left running, `agents.test.mjs` (`node:test`) starts
protection with a RedPill profile and checks, for each agent:

1. it is detected;
2. after a user setting is written to its configuration and
   `pap agents connect <id> --model z-ai/glm-5.3-flash`, the agent's one-shot
   command replies `PAP-OK` through its default model (surrounding quotes,
   `*`, backticks and trailing punctuation are ignored);
3. every usage record the reply created is HTTP 200 and Verified;
4. `pap agents disconnect` keeps that user setting and restores the rest of
   the agent's configuration files, apart from the provider definitions kept
   by design;
5. the agent's old token gets 401.

With the agents that passed connected again, it switches to an imported
profile without a credential, which protection cannot verify (`pap profiles
use` fails with `invalid_state`, and the phase is `profileRequired`). Each
agent's configuration must stay unchanged and none of its requests may leave
the device. Agent tokens are withdrawn until protection is verified again, so
their inference requests get 401 `unauthorized`. The Local API's own client
token gets 503 `gateway_not_verified`. Switching back to RedPill must
restore protection, the tokens and a reply. Finally, `pap stop` must restore every agent and
revoke every token, and `pap service stop` must leave them restored.

The output ends with one row per agent and the result of each phase.

## Run locally

Requirements: Docker, `sha256sum`, and the GitHub CLI for release downloads.
Nothing is built: the container is removed afterwards, and only the
`ubuntu:24.04` image stays.

```sh
export PAP_E2E_API_KEY=...          # a RedPill API key
apps/desktop/e2e/run.sh             # the newest desktop-v* release
apps/desktop/e2e/run.sh --tag desktop-v0.2.0-beta.11
apps/desktop/e2e/run.sh --deb path/to/private-ai-proxy-0.2.0-linux-x64.deb
unset PAP_E2E_API_KEY
```

`--deb` needs the package's `SHA256SUMS` in the same directory. The key is
piped to the test's stdin and from there only to `pap profiles add
--key-stdin` inside the container: it is never an argument, printed, or
written outside the container. The processes that see it (`node` and `pap`)
run from root-owned absolute paths with the system `PATH`; only the agents
search the directories their installers write to (`AGENT_PATH`). A run
takes about 10 minutes and sends about a dozen tiny prompts.

## Versions

`versions.env` pins Node.js and every agent, one line each, so a bump is a
one-line change. `npm_config_before` freezes the npm dependencies the
installers resolve; move it forward with any bump. Codex must stay on the
baseline the app supports (`agent-bridge/resources/codex/manifest.json`).
Hermes is pinned to a release tag and installed with that tag's own
`scripts/install.sh`, since the current installer expects the current source
tree; its Python dependencies come from the tag's hash-verified `uv.lock`.
Claude Code and OpenCode self-updates are off (`DISABLE_AUTOUPDATER`,
`OPENCODE_DISABLE_AUTOUPDATE`).

The installer scripts are not checksummed. Apart from Hermes', they are
served from unversioned URLs and change independently of the agent release
they install, so a pinned checksum would fail on unrelated installer edits.
The installed version check catches an installer that ignores its pin.
