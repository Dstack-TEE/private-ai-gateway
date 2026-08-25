# @phala/pi-provider-aci

Vendor-neutral **Pi** provider for [private-ai-gateway] (the ACI protocol),
with **attested TLS (SPKI) pinning** as the security control.

This is the neutral core. Branded distributions add their identity on top and
publish their own npm packages:

- [`pi-provider-redpill`](https://www.npmjs.com/package/pi-provider-redpill) — Redpill AI
- [`pi-provider-phala-cloud`](https://www.npmjs.com/package/pi-provider-phala-cloud) — Phala Cloud

Both are thin skins over this package (`createProvider` with an instance-scoped
brand profile). Multiple branded providers can coexist in one process without
sharing provider ids, environment names, config paths, fallback models or
connection state.

## Threat model: prevention, not audit

This plugin's job is **prevention**: make sure the request and response are
readable only by the attested workload. It does NOT do per-response receipt
verification. The gateway still stamps `x-receipt-id` on every reply and serves
signed receipts — those are a post-hoc audit trail, and this plugin deliberately
does not consume them. If you want audit, verify receipts with the repo's
reference verifier ([`@phala/aci-verifier`](../verifier-ts)) directly.

## Use from this repository

`@phala/pi-provider-aci` and `@phala/aci-verifier` are not published to npm
yet. From a source checkout, install both client workspaces and load the Pi
extension directly:

```bash
npm --prefix clients/verifier-ts ci
npm --prefix clients/pi-provider ci
export ACI_BASE_URL=https://<your-gateway>/v1   # your private-ai-gateway endpoint
export ACI_LLM_API_KEY=...
pi -e clients/pi-provider/packages/pi-provider-aci
```

After the release boundary in [PLAN.md](PLAN.md) is complete, the intended
install command is `pi install npm:@phala/pi-provider-aci`.

## What it adds

- OpenAI-compatible provider (`aci`) with live model discovery from
  `/v1/models` (no hardcoded catalog).
- `is_tee` filtering — only confidentially-served models are registered by default.
- **Attested TLS (SPKI) pinning.** At session start the gateway attestation is
  fetched and validated, then the TLS connection is pinned to the attested
  `workload_keyset.tls_public_keys` SPKI. **Fail closed by default**: with
  `pinning.enabled` (the default) an unpinnable session blocks inference rather
  than silently downgrading to plain CA-TLS — the `failOpenOnUnpinned` setting
  opts into the old footer-warning behavior.
- `/aci-settings`, `/attestation`, `/aci-receipt` and `/aci-session` commands.
  The latter two are an opt-in audit trail: `x-receipt-id` is captured (not
  verified) from each response, and the user can show the receipt document or
  an attested session on demand with `/aci-receipt [id]` / `/aci-session <id>`
  (`/attestation` shows the pinned report: keyset digest, binding, keys, expiry).

## How the verified connection is established

The provider creates an instance-scoped connection with `connectAci()` from
`@phala/aci-verifier/node`. The shared client:

- recomputes the keyset digest from the served keyset (not trusted from the
  report),
- checks `report_data` binds our fresh nonce,
- checks `not_after`,
- verifies the TDX quote to the Intel root and confirms it binds the same
  `report_data`, and
- opens a normal hostname-validated TLS connection whose peer SPKI must match
  `workload_keyset.tls_public_keys`.

Only a report that passes both binding and hardware verification yields an
SPKI pin, so the pin is **attested** — not trust-on-first-use.

Pi's `openai-completions` adapter accepts a custom `fetch`, so the provider
injects the connection's scoped fetch through `StreamOptions.fetch`. It never
changes `globalThis.fetch`: unrelated providers, tools, MCP servers and
telemetry keep their own transports. A failed or expired connection blocks
model traffic unless the user explicitly enables fail-open behavior. Each Pi
session gets a fresh connection and closes it on shutdown.

The same `connectAci()` transport works with other Node SDKs and agent
frameworks; see the [verifier integration examples](../verifier-ts/README.md#node-sdk-and-agent-frameworks).

## Branding / profiles

`createProvider(profile)` registers the provider with a brand identity:

```ts
import { createProvider } from "@phala/pi-provider-aci";
export default createProvider({
  providerId: "my-brand",
  label: "My Brand",
  defaultBaseUrl: "https://gateway.example/v1",
  apiKeyEnv: "MY_LLM_API_KEY",
  envPrefix: "MY",
  fallbackModels: [...],
});
```

[private-ai-gateway]: https://github.com/Dstack-TEE/private-ai-gateway
