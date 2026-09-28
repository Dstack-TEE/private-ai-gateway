# Live end-to-end test

Tests a published Linux x64 desktop package against the live RedPill service
with every supported coding agent: Claude Code, Codex, OpenCode, Pi, Oh My Pi,
OpenClaw, Hermes, Qwen Code and Kilo CLI. An agent the package predates (it is missing
from `pap agents list`) is skipped and shows `skip` in the summary. `Desktop live E2E` (`.github/workflows/desktop-e2e.yml`)
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

It also saves a second profile, `redpill-alt`, for the same service and key,
and switches back to `redpill` if adding it changed the active profile. With
the agents that passed connected again, the test asks `run.sh` to disconnect
the container from its Docker network, confirms the service is unreachable,
and runs `pap profiles use redpill-alt`, which restarts protection on a
profile it cannot verify now. The phase must be `reconnecting` or
`interrupted`, each agent's configuration must still point at the Local API,
every agent token and the Local API client token must get 503
`gateway_not_verified`, and no request may leave the device. Releases up to
0.2.0-beta.11 withdraw agent tokens during an outage and answer 401 instead,
so that check fails for them. After the network is reconnected, protection
must be Protected again within three minutes, with the tokens accepted and a
real reply through one agent. Finally, `pap stop` must restore every agent
and revoke every token, and `pap service stop` must leave them restored.

When an agent's one-shot command times out or does not reply `PAP-OK`, the
failure shows why: the last 40 lines of its stdout and stderr and its latest
usage record since the prompt (HTTP status, verification and detail), with the
API key, agent tokens and anything shaped like a credential masked.

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
apps/desktop/e2e/run.sh --agents opencode --opencode-v2   # OpenCode 2 only
unset PAP_E2E_API_KEY
```

`--agents` installs and tests only the listed agent ids. `--opencode-v2`
installs OpenCode 2 with its own installer instead of OpenCode 1; both provide
the `opencode` command, so the workflow tests OpenCode 2 in a second job with
`--agents opencode`.

`--deb` needs the package's `SHA256SUMS` in the same directory. The key is
piped to the test's stdin and from there only to `pap profiles add
--key-stdin` inside the container: it is never an argument, printed, or
written outside the container. The processes that see it (`node` and `pap`)
run from root-owned absolute paths with the system `PATH`; only the agents
search the directories their installers write to (`AGENT_PATH`). A run
takes about 10 minutes and sends about a dozen tiny prompts.

## Versions

`versions.env` pins Node.js and every agent, one line each, so a bump is a
one-line change. `OPENCODE_VERSION` pins OpenCode 1 and `OPENCODE_V2_VERSION`
OpenCode 2. `npm_config_before` freezes the npm dependencies the
installers resolve; move it forward with any bump. Codex must stay on the
baseline the app supports (`agent-bridge/resources/codex/manifest.json`).
Hermes is pinned to a release tag and installed with that tag's own
`scripts/install.sh`, since the current installer expects the current source
tree; its Python dependencies come from the tag's hash-verified `uv.lock`.
Claude Code, OpenCode and Kilo self-updates are off (`DISABLE_AUTOUPDATER`,
`OPENCODE_DISABLE_AUTOUPDATE`, `KILO_DISABLE_AUTOUPDATE`).

The installer scripts are not checksummed. Apart from Hermes', they are
served from unversioned URLs and change independently of the agent release
they install, so a pinned checksum would fail on unrelated installer edits.
The installed version check catches an installer that ignores its pin.
