# Coding agents over ACI

Coding-agent integrations have one rule: verification belongs at the transport
boundary, not in agent-specific verification code. A plugin may wire the
shared transport into an agent, but it must not reimplement ACI.

```text
fetch-aware Node/Bun client ---- connectAci() ---- ACI gateway

base-URL-only CLI ----------- aci serve ------- ACI gateway
                            127.0.0.1:4180
```

`connectAci()` is the direct integration when an application accepts a custom
`fetch`; its conditional runtime entry selects the Node or Bun adapter.
Applications that expose only a base URL use `aci serve`: one local process
verifies the hardware-backed workload identity, pins the remote TLS SPKI, and
forwards the agent's native HTTP surface. It does not translate API protocols.

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

OpenCode runs plugins under Bun and its provider factory accepts an
`options.fetch` function. JSON cannot contain a function, so a small local
plugin injects the shared ACI client. First add the verifier dependency:

```json
{
  "dependencies": {
    "@phala/aci-verifier": "^0.2.2"
  }
}
```

Save that as `.opencode/package.json`; OpenCode installs local-plugin
dependencies with Bun. Configure the remote provider normally:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "aci": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "ACI",
      "options": {
        "baseURL": "{env:ACI_BASE_URL}",
        "apiKey": "{env:ACI_API_KEY}"
      },
      "models": {
        "<model-id>": { "name": "<model-id>" }
      }
    }
  }
}
```

Then place this adapter at `.opencode/plugins/aci.ts`:

```ts
import type { Plugin } from '@opencode-ai/plugin';
import {
  connectAci,
  type AciConnection,
} from '@phala/aci-verifier/runtime';

export const AciPlugin: Plugin = async () => {
  let connection: AciConnection | undefined;

  return {
    async config(config) {
      const provider = config.provider?.aci;
      const options = provider?.options;
      const baseURL = options?.baseURL;
      const apiKey = options?.apiKey;
      if (!provider || typeof baseURL !== 'string' || typeof apiKey !== 'string') {
        throw new Error('ACI provider requires string baseURL and apiKey options');
      }

      const next = await connectAci({
        baseURL,
        policy: {
          requireProductionOs: true,
          acceptedComposeHashes: ['<reviewed-sha256-app-compose>'],
        },
      });
      if (!next.identity.transcript.verdict.verified) {
        await next.close();
        throw new Error(next.identity.transcript.verdict.line);
      }
      provider.options = { ...options, baseURL: next.baseURL, fetch: next.fetch };

      const previous = connection;
      connection = next;
      await previous?.close();
    },
    async dispose() {
      await connection?.close();
    },
  };
};
```

Replace the compose placeholder with a hash published by the reviewed release
pipeline; never learn it from the endpoint being verified. Importing
`/runtime` selects the Bun adapter, whose TLS callback is covered by the same
pinned-channel contract test as the Node adapter. The plugin only performs
dependency injection and lifecycle cleanup; quote, SPKI, policy, digest,
receipt, and session verification remain in `connectAci()`.

For a route that specifically implements `/v1/responses`, OpenCode documents
`@ai-sdk/openai` instead of `@ai-sdk/openai-compatible`.

OpenCode can still point at `aci serve` when an operator wants one shared local
process for several tools. That is an operational choice, not a Bun
compatibility requirement.

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
- [Bun fetch TLS and proxy options](https://bun.com/docs/runtime/networking/fetch)
