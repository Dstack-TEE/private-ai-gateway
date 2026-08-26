# Coding agents over ACI

Coding-agent integrations have one rule: verification belongs at the transport
boundary, not in an agent-specific plugin.

```text
fetch-aware Node client ---- connectAci() ---- ACI gateway

coding-agent CLI ---------- aci serve ------- ACI gateway
                         127.0.0.1:4180
```

`connectAci()` is the smaller integration when an application accepts a custom
`fetch`. Standalone coding agents generally expose only a base URL, so they use
`aci serve`: one local process verifies the hardware-backed workload identity,
pins the remote TLS SPKI, and forwards the agent's native HTTP surface. It does
not translate API protocols.

Both paths demand `provider.aci_verified` on JSON inference requests by
default, record exact request/response digests without buffering streams, and
verify signed receipts plus cited sessions on demand. A configured session set
is a local acceptance policy: request pins are intersected with it, and a
disjoint request fails before reaching the gateway.

## Start the local verifier

From this repository:

```bash
cargo run --bin aci -- serve https://gateway.example.com \
  --accept-compose <reviewed-sha256-app-compose>
```

The command verifies before listening and fails closed. `--accept-compose` is
repeatable for a controlled release rotation. Without it, `aci serve` verifies
and reports the measured compose but does not claim that the deployment is a
reviewed release. Never populate this value from the first endpoint response.
The agent-facing base URLs are:

- OpenAI APIs: `http://127.0.0.1:4180/v1`
- Anthropic Messages: `http://127.0.0.1:4180`

Keep the gateway key in an environment variable. `aci serve` passes request
authentication through and does not need the key in its command line.

## Compatibility

| Agent | Agent protocol | Integration | Status |
| --- | --- | --- | --- |
| Pi | OpenAI Chat Completions | Inject `connectAci().fetch` | Native |
| Codex CLI | OpenAI Responses | Point a custom model provider at `aci serve` | Supported when the selected gateway route serves `/v1/responses` |
| Claude Code | Anthropic Messages | Set `ANTHROPIC_BASE_URL` to `aci serve` | Supported; `/v1/messages/count_tokens` is optional |
| OpenCode | OpenAI Chat Completions or Responses | Configure an AI SDK provider against `aci serve` | Supported |

### Codex CLI

Codex custom providers use the Responses API. Put the provider in the
user-level `$CODEX_HOME/config.toml`; project-local config cannot override
provider definitions.

```toml
model = "<model-id>"
model_provider = "aci"

[model_providers.aci]
name = "ACI local verifier"
base_url = "http://127.0.0.1:4180/v1"
env_key = "ACI_API_KEY"
wire_api = "responses"
```

The gateway model route must implement `/v1/responses`; Codex does not support
falling back to Chat Completions.

### Claude Code

Claude Code speaks Anthropic Messages to a custom gateway:

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:4180
export ANTHROPIC_AUTH_TOKEN="$ACI_API_KEY"
export ANTHROPIC_MODEL=<model-id>
claude
```

The gateway already exposes `/v1/messages`. Claude Code may also call
`/v1/messages/count_tokens`; that endpoint is optional and Claude Code falls
back when it is unavailable. New Claude Code releases may add beta headers and
body fields, so compatibility should be exercised when either side upgrades.

### OpenCode

Use the OpenAI-compatible provider for Chat Completions:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "aci": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "ACI",
      "options": {
        "baseURL": "http://127.0.0.1:4180/v1",
        "apiKey": "{env:ACI_API_KEY}"
      },
      "models": {
        "<model-id>": { "name": "<model-id>" }
      }
    }
  }
}
```

For a route that specifically implements `/v1/responses`, OpenCode documents
`@ai-sdk/openai` instead of `@ai-sdk/openai-compatible`.

OpenCode's internal provider factory accepts a JavaScript `options.fetch`, and
a plugin can mutate provider options. Its JSON configuration cannot express a
function, however, and the plugin runtime is Bun while the current
`connectAci()` transport relies on Node's undici dispatcher for the verified
TLS callback. Until the same channel-binding tests pass in Bun, routing
OpenCode through `aci serve` is the fail-closed integration rather than a
nominal direct adapter whose SPKI check may not execute.

## Trust boundary

`aci serve` and `connectAci()` share the same trust contract: fresh quote and
keyset binding, measured compose appraisal, optional reviewed compose
allowlist, identity expiry, hostname/SPKI channel binding, verified-serving
constraints, and receipt/session auditing. Agent adapters only select an HTTP
protocol and transport; they do not redefine verification.

These configurations protect model HTTP traffic between the local verifier and
the attested gateway. They do not automatically cover WebSockets, MCP servers,
tool calls, browser automation, shell commands, extension traffic or agent
telemetry. The local agent and `aci serve` necessarily see plaintext prompts
and responses; the remote network path is the part bound to the attested TLS
identity.

## Sources

Checked 2026-08-26:

- [Codex custom model providers](https://learn.chatgpt.com/docs/config-file/config-advanced#custom-model-providers)
  and [configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference#configtoml)
- [Claude Code gateway connection](https://code.claude.com/docs/en/llm-gateway-connect)
  and [protocol reference](https://code.claude.com/docs/en/llm-gateway-protocol)
- [OpenCode providers](https://opencode.ai/docs/providers/) and
  [plugins](https://opencode.ai/docs/plugins/), source commit
  [`fd9bd44`](https://github.com/anomalyco/opencode/commit/fd9bd448a2e68990e7aed3495e5590cecb934bfb)
