# Coding agents over ACI

Coding agents should integrate ACI through capabilities officially exposed by
their host. The adapter owns only ACI-specific verification and model mapping;
the host continues to own installation, authentication, model selection,
persistence, and lifecycle.

| Host | Official extension point | ACI integration |
| --- | --- | --- |
| Pi | Provider extension, auth API, model registry, commands, footer | `pi-provider-redpill`, `pi-provider-phala-cloud`, or `@phala/pi-provider-aci` |
| OpenCode | Server plugin, provider config, auth hooks, tools, dispose | `opencode-provider-redpill`, `opencode-provider-phala-cloud`, or `@phala/opencode-provider-aci` |
| Node/Bun SDK application | Per-client custom `fetch` | `connectAci().fetch` |
| Base-URL-only coding agent | No transport injection point | No native ACI adapter in this release |

All supported paths share `@phala/aci-provider` and
`@phala/aci-verifier`. They verify the workload and TLS channel before sending
model traffic, discover the live model catalog, require verified serving,
retain bounded wire digests, and verify every signed receipt and cited session
before an inference response finishes.

## Pi

Install one provider through Pi's package manager:

```sh
pi install npm:pi-provider-redpill
# or
pi install npm:pi-provider-phala-cloud
```

Then use Pi's native login and model picker:

```text
/login redpill
# paste the Redpill API key

# or
/login phala
# approve the Phala Cloud device login

# wait for the footer to show aci-verified
/model
# search for redpill/ or phala/, select a model, and press Ctrl+S
```

Pi owns persistence. It writes credentials to `~/.pi/agent/auth.json`, dynamic
catalogs to `~/.pi/agent/models-store.json`, and a default selected with
`Ctrl+S` to `~/.pi/agent/settings.json`. A normal model selection changes only
the current session. Cached dynamic models are restored offline.

`REDPILL_AI_API_KEY` and `PHALA_AI_API_KEY` can provide a credential to the
current process. Pi intentionally does not copy environment variables into its
credential store; use `/login` when the key must survive a restart.

The provider-scoped commands expose ACI-specific state that Pi does not know
about: settings, attestation, retained receipts, and content-addressed sessions.
For example, Redpill registers `/redpill-settings`, `/redpill-attestation`,
`/redpill-receipt`, and `/redpill-session`.

## OpenCode

Install a branded provider through OpenCode's native plugin command:

```sh
opencode plugin opencode-provider-redpill --global
opencode providers login --provider redpill

# or
opencode plugin opencode-provider-phala-cloud --global
opencode providers login --provider phala
```

Omit `--global` for a project installation. The plugin command persists the
plugin entry in OpenCode configuration, and provider login persists the
credential in OpenCode's auth store. Do not add a separate provider block: the
plugin owns the provider, verified fetch, live models, and auth loader.

Redpill currently supports API keys only. Phala Cloud offers both its device
account flow and an API-key method. The device flow returns the issued
Confidential AI key through OpenCode's documented browser-authorization hook;
the plugin does not create a parallel OAuth token lifecycle or credential file.

`REDPILL_AI_API_KEY` and `PHALA_AI_API_KEY` are supported for the current
process. OpenCode does not copy environment variables into its auth store.

For another ACI gateway, configure the neutral plugin:

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

Then run `opencode providers login --provider aci` or set `ACI_API_KEY`.
The read-only `aci_inspect`, `redpill_aci_inspect`, or `phala_aci_inspect` tool
reports connection status, attestation, receipt history, receipt audits, and
session audits without returning prompts, responses, or raw evidence.

## SDK applications

Applications that expose a per-client fetch hook can inject the verified
transport directly:

```ts
import { connectAci } from "@phala/aci-verifier/runtime";

const aci = await connectAci({
  baseURL: "https://gateway.example.com/v1",
  acceptedComposeHashes: ["<reviewed-compose-sha256>"],
});

const response = await aci.fetch("https://gateway.example.com/v1/models");
```

Node and Bun expose the same API. Conditional package exports select the
runtime-specific TLS adapter; application and framework code should not branch
on the runtime. Prefer `@phala/aci-provider` when the application also needs
shared model discovery, capability mapping, receipt history, and response
completion verification.

## Unsupported hosts

A custom API base URL is not enough to inject ACI's attested TLS transport. A
coding agent without an official custom-fetch or provider-plugin extension
point therefore has no native integration in this release. In particular, this
guide does not claim native Codex CLI or Claude Code support. Add a dedicated
adapter only when the host exposes a supported transport boundary and lifecycle
contract.

## Trust boundary

The shared clients verify a fresh quote and nonce-bound keyset, measured
compose, optional reviewed-release allowlist, identity expiry, hostname and TLS
SPKI binding, verified-serving constraints, exact wire digests, signed receipts,
and cited sessions. Any required failure blocks the request or response.

Coverage ends at model HTTP traffic between the local client and the attested
gateway. WebSockets, MCP servers, tools, browser automation, shell commands,
extensions, and telemetry have separate trust boundaries. The local coding
agent still sees plaintext prompts and responses; ACI binds the remote network
path to the attested workload identity.

## Sources

Checked 2026-08-28:

- [Pi providers](https://github.com/earendil-works/pi-mono/blob/main/packages/coding-agent/docs/providers.md),
  [custom providers](https://github.com/earendil-works/pi-mono/blob/main/packages/coding-agent/docs/custom-provider.md),
  and [models and thinking](https://github.com/earendil-works/pi-mono/blob/main/packages/coding-agent/docs/keybindings.md#models-and-thinking)
- [OpenCode providers](https://opencode.ai/docs/providers/) and
  [plugins](https://opencode.ai/docs/plugins/), source tag
  [`v1.18.23`](https://github.com/anomalyco/opencode/tree/v1.18.23)
- [Bun fetch TLS options](https://bun.com/docs/runtime/networking/fetch)
