# @phala/pi-provider-aci

Vendor-neutral **Pi** provider for [private-ai-gateway] (the ACI protocol),
with attested TLS (SPKI) pinning and per-response receipt verification.

This is the neutral core. Branded distributions add their identity on top and
publish their own npm packages:

- [`pi-provider-redpill`](https://www.npmjs.com/package/pi-provider-redpill) — Redpill AI
- [`pi-provider-phala-cloud`](https://www.npmjs.com/package/pi-provider-phala-cloud) — Phala Cloud

Both are thin skins over this package (`createProvider` with a brand profile) —
they are interchangeable, and the underlying protocol code lives here once.

## Install (core)

```bash
pi install npm:@phala/pi-provider-aci
export ACI_BASE_URL=https://<your-gateway>/v1   # your private-ai-gateway endpoint
export ACI_LLM_API_KEY=...
```

## What it adds

- OpenAI-compatible provider (`aci`) with live model discovery from
  `/v1/models` (no hardcoded catalog).
- `is_tee` filtering — only confidentially-served models are registered by default.
- **Attested TLS (SPKI) pinning** — at session start the gateway attestation is
  validated and the TLS connection is pinned to the attested
  `workload_keyset.tls_public_keys` SPKI (request + response are only readable
  by the attested workload). Fails closed on mismatch.
- **Per-response receipt verification** — the footer shows
  `verified` / `verified*` / `routed` / `attested` / `mismatch` after each
  reply, based on the signed receipt (`upstream.verified`) and ed25519
  signature verification over the canonical receipt.
- `/aci-settings` and `/attestation` commands.

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