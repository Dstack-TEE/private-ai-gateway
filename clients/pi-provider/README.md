# @phala/pi-provider-aci

Vendor-neutral **Pi** provider for [private-ai-gateway] (the ACI protocol),
with **attested TLS (SPKI) pinning** as the security control.

This is the neutral Pi adapter. Branded distributions add their identity on top and
publish their own npm packages:

- [`pi-provider-redpill`](https://www.npmjs.com/package/pi-provider-redpill) — Redpill AI
- [`pi-provider-phala-cloud`](https://www.npmjs.com/package/pi-provider-phala-cloud) — Phala Cloud

Both are thin skins over this package (`createProvider` with an instance-scoped
brand profile). Multiple branded providers can coexist in one process without
sharing provider ids, environment names, config paths, fallback models or
connection state.

## Threat model: prevention plus on-demand audit

Every model request first passes through the verified, SPKI-pinned transport.
That is the prevention boundary. The same transport records bounded request and
response wire digests without buffering the SSE stream. `/aci-receipt [id]`
then fetches the signed receipt and cited session on demand and verifies the
signature, keyset binding, both body hashes, serving mode, session integrity,
validity window, evidence digest, and configured session pins.

Receipt verification is deliberately on demand rather than an extra network
round trip on every inference. An exchange that is no longer in the bounded
history cannot receive a complete body-hash audit and fails explicitly.

## Install

Install the published package:

```bash
pi install npm:@phala/pi-provider-aci
```

For a source checkout:

```bash
npm --prefix clients ci
npm --prefix clients run build
export ACI_BASE_URL=https://<your-gateway>/v1   # your private-ai-gateway endpoint
export ACI_API_KEY=...
pi -e clients/pi-provider/packages/pi-provider-aci
```

## What it adds

- OpenAI-compatible provider (`aci`) with live model discovery from
  `/v1/models` (no hardcoded catalog).
- `is_tee` filtering — only confidentially-served models are registered by default.
- **Attested TLS (SPKI) pinning.** At session start the gateway attestation is
  fetched and validated, then the TLS connection is pinned to the attested
  `workload_keyset.tls_public_keys` SPKI. This is an invariant of the provider,
  not a setting: an unverified or unpinnable session blocks inference rather
  than silently downgrading to plain CA-TLS.
- **Optional reviewed-release pinning.** Set
  `<PREFIX>_ACCEPTED_COMPOSE_HASHES` to a comma-separated list of reviewed
  `sha256(app_compose)` values, or put them under
  `trust.acceptedComposeHashes` in the provider config. The measured compose is
  always verified; an allowlist additionally rejects unreviewed deployments.
- **Optional attested-session policy.** Set
  `<PREFIX>_ACCEPTED_SESSION_IDS` or `trust.acceptedSessionIds` to a non-empty
  list of audited session ids. Request-supplied pins are intersected with this
  local set; a disjoint request fails before network access.
- `/aci-settings`, `/attestation`, `/aci-receipt` and `/aci-session` commands.
  `/aci-receipt [id]` runs the complete recorded-exchange audit described
  above; `/aci-session <id>` can inspect a session directly. `/attestation`
  shows the pinned report, keyset digest, binding, keys, and expiry.

## How the verified connection is established

The adapter creates an instance-scoped `@phala/aci-provider`. That shared
provider uses the Node ACI transport to:

- recomputes the keyset digest from the served keyset (not trusted from the
  report),
- checks `report_data` binds our fresh nonce,
- checks `not_after`,
- verifies the TDX quote to the Intel root and confirms it binds the same
  `report_data`, and
- verifies `sha256(app_compose)` is measured into RTMR3 and, when configured,
  belongs to the reviewed compose allowlist, and
- opens a normal hostname-validated TLS connection whose peer SPKI must match
  `workload_keyset.tls_public_keys`.

Only a report that passes both binding and hardware verification yields an
SPKI pin, so the pin is **attested** — not trust-on-first-use.

Hardware verification alone says that some real TDX workload owns the TLS key;
it does not identify a reviewed gateway release. Branded packages should ship
their reviewed compose hashes through `acceptedComposeHashes` when their
release pipeline publishes them. Until then `/attestation` explicitly reports
`measurement verified, not pinned`.

Pi's `openai-completions` adapter receives the connection's scoped fetch through
`StreamOptions.fetch`. A failed or expired connection blocks model traffic.
Each Pi session gets a fresh connection and closes it on shutdown.

The same `connectAci()` API works with other Node and Bun SDKs and agent
frameworks; see the [verifier integration examples](../verifier-ts/README.md#runtime-sdk-and-agent-frameworks).
Coding-agent CLIs without a custom fetch hook use the local `aci serve` proxy;
see the [coding agent guide](../coding-agents.md).

## Branding / profiles

`createProvider(profile)` registers the provider with a brand identity:

```ts
import { createProvider } from "@phala/pi-provider-aci";
export default createProvider({
  providerId: "my-brand",
  label: "My Brand",
  defaultBaseURL: "https://gateway.example/v1",
  apiKeyEnv: "MY_LLM_API_KEY",
  envPrefix: "MY",
  logPrefix: "[my-brand]",
  acceptedComposeHashes: ["<reviewed-sha256-app-compose>"],
  catalog: [...],
  footerKey: "my-brand",
});
```

[private-ai-gateway]: https://github.com/Dstack-TEE/private-ai-gateway
