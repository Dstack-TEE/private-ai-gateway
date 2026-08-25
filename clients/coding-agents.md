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

## Start the local verifier

From this repository:

```bash
cargo run --bin aci -- serve https://gateway.example.com
```

The command verifies before listening and fails closed. The agent-facing base
URLs are:

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
| Aider | OpenAI Chat Completions | Set its OpenAI-compatible API base to `aci serve` | Supported |
| Gemini CLI | Gemini `generateContent` | No compatible ACI gateway surface today | Not yet supported |

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

### Aider

```bash
export OPENAI_API_BASE=http://127.0.0.1:4180/v1
export OPENAI_API_KEY="$ACI_API_KEY"
aider --model openai/<model-id>
```

### Gemini CLI

Gemini CLI can override `GOOGLE_GEMINI_BASE_URL`, including with a localhost
URL, but it still sends the Gemini API shape such as `models/*:generateContent`.
`aci serve` preserves paths and bodies; it does not translate that protocol to
OpenAI or Anthropic. Supporting Gemini CLI therefore needs a real Gemini API
surface in private-ai-gateway (including streaming, tools and error semantics),
or an upstream Gemini CLI provider interface. A per-agent verifier would only
duplicate security code and would not solve the protocol mismatch.

## Trust boundary

These configurations protect model HTTP traffic between the local verifier and
the attested gateway. They do not automatically cover WebSockets, MCP servers,
tool calls, browser automation, shell commands, extension traffic or agent
telemetry. The local agent and `aci serve` necessarily see plaintext prompts
and responses; the remote network path is the part bound to the attested TLS
identity.

## Sources

Checked 2026-08-25:

- [Codex custom model providers](https://learn.chatgpt.com/docs/config-file/config-advanced#custom-model-providers)
  and [configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference#configtoml)
- [Claude Code gateway connection](https://code.claude.com/docs/en/llm-gateway-connect)
  and [protocol reference](https://code.claude.com/docs/en/llm-gateway-protocol)
- [OpenCode providers](https://opencode.ai/docs/providers/), source commit
  [`a7444bf`](https://github.com/anomalyco/opencode/commit/a7444bf944c219b9eaba2f794847b3001237795f)
- [Aider OpenAI-compatible APIs](https://aider.chat/docs/llms/openai-compat.html),
  source commit [`5dc9490`](https://github.com/Aider-AI/aider/commit/5dc9490bb35f9729ef2c95d00a19ccd30c26339c)
- [Gemini CLI configuration](https://github.com/google-gemini/gemini-cli/blob/812f7a2bcf20b6e80e2e50c3c8fa8e26567bc1e8/docs/reference/configuration.md)
