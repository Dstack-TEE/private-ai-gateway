# Coding agents over ACI

Coding-agent integrations keep verification at the transport boundary. Native
providers also own model discovery and host-specific lifecycle integration.

```text
fetch-aware Node/Bun client ---- connectAci() ---- ACI gateway

base-URL-only CLI ----------- aci serve ------- ACI gateway
                            127.0.0.1:4180
```

`connectAci()` is the direct integration when an application accepts a custom
`fetch`; its conditional runtime entry selects the Node or Bun adapter.
Applications that expose only a base URL use `aci serve`: one local process
verifies the hardware-backed workload identity, pins the remote TLS SPKI, and
forwards the agent's protocol unchanged. The selected gateway route must serve
that protocol.

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
repeatable for a controlled release rotation. Omitting it verifies the measured
compose without accepting a reviewed release. Populate it from authenticated
release metadata, not the endpoint being verified.
The agent-facing base URLs are:

- OpenAI APIs: `http://127.0.0.1:4180/v1`
- Anthropic Messages: `http://127.0.0.1:4180`

Keep the gateway key in an environment variable. Authentication passes through
from the agent request; the `aci serve` command line contains no key.

## Compatibility

| Agent | Agent protocol | Integration | Status |
| --- | --- | --- | --- |
| Pi | OpenAI Chat Completions | Inject `connectAci().fetch` | Native |
| Codex CLI | OpenAI Responses | Point a custom model provider at `aci serve` | Supported when the selected gateway route serves `/v1/responses` |
| Claude Code | Anthropic Messages | Set `ANTHROPIC_BASE_URL` to `aci serve` | Supported; `/v1/messages/count_tokens` is optional |
| OpenCode | OpenAI Chat Completions or Responses | Plugin injects Bun `connectAci().fetch` | Native |

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

The gateway model route must implement `/v1/responses`.

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

For RedPill, add one plugin entry to `opencode.json`:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "plugin": ["opencode-provider-redpill"]
}
```

Set `REDPILL_LLM_API_KEY` or run `opencode providers login`, then choose
`redpill/<model-id>`. The plugin creates the provider itself; do not add a
separate `provider.redpill` block.

For another ACI gateway, use the neutral plugin:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "plugin": [
    [
      "@phala/opencode-provider-aci",
      {
        "baseURL": "https://gateway.example.com/v1",
        "trust": {
          "acceptedComposeHashes": ["<reviewed-compose-sha256>"]
        }
      }
    ]
  ]
}
```

The plugin discovers `/v1/models` over the verified connection, defaults to
`is_tee: true`, excludes embedding-only entries, maps model limits, prices,
modalities, tools and reasoning controls, and accepts an optional model
allowlist. The provider is installed with a rejecting fetch before any async
verification starts. OpenCode currently ignores `config()` hook errors, so
this ordering is required to prevent an ordinary HTTPS downgrade. Each
inference response is held open until its signed receipt and cited session
verify.

For a route that specifically implements `/v1/responses`, OpenCode documents
`@ai-sdk/openai` instead of `@ai-sdk/openai-compatible`.

OpenCode can still point at `aci serve` when an operator wants one shared local
process for several tools. That is an operational choice, not a Bun
compatibility requirement.

## Trust boundary

`aci serve` and `connectAci()` share the same trust contract: fresh quote and
keyset binding, measured compose appraisal, optional reviewed compose
allowlist, identity expiry, hostname/SPKI channel binding, verified-serving
constraints, and receipt/session auditing. Agent adapters select the HTTP
protocol and transport; verification remains in the shared client.

Coverage ends at model HTTP traffic between the local verifier and attested
gateway. WebSockets, MCP servers, tools, browser automation, shell commands,
extensions, and telemetry have separate trust boundaries. The local agent and
`aci serve` see plaintext prompts and responses; ACI binds the remote network
path to the attested TLS identity.

## Sources

Checked 2026-08-27:

- [Codex custom model providers](https://learn.chatgpt.com/docs/config-file/config-advanced#custom-model-providers)
  and [configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference#configtoml)
- [Claude Code gateway connection](https://code.claude.com/docs/en/llm-gateway-connect)
  and [protocol reference](https://code.claude.com/docs/en/llm-gateway-protocol)
- [OpenCode providers](https://opencode.ai/docs/providers/) and
  [plugins](https://opencode.ai/docs/plugins/), source commit
  tag [`v1.18.23`](https://github.com/anomalyco/opencode/tree/v1.18.23)
- [Bun fetch TLS and proxy options](https://bun.com/docs/runtime/networking/fetch)
