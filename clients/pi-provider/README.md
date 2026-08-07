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
  `workload_keyset.tls_public_keys` SPKI. **Fail closed by default**: with
  `pinning.enabled` (the default) an unpinnable session blocks inference rather
  than silently downgrading to plain CA-TLS; the `/aci-settings` toggle
  `failOpenOnUnpinned` opts into the old footer-warning behavior.
- **Per-response receipt verification** — the footer shows
  `verified` / `verified*` / `routed` / `attested` / `mismatch` after each
  reply, backed by the repo's reference verifier
  ([`@phala/aci-verifier`](../verifier-ts), `clients/verifier-ts`) — this is
  the *same* code that ships with the gateway, not a private reimplementation.
  `verified` means the receipt signature validated AND the body hashes were
  checked against the bytes we saw; `verified*` means the signature validated
  but body-hash bytes were not available inside pi's extension surface (pi does
  not expose the raw response stream to extensions) — we say so rather than
  overclaim. `mismatch` means a signature FAILED or the keyset did not match.
- `/aci-settings` and `/attestation` commands.

## How verification is wired

`src/aci-client.ts` fetches the ACI artifacts and delegates all cryptographic
verification to `@phala/aci-verifier`:

- report binding (`verifyReportBinding`) — keyset digest, `report_data`,
  `not_after`;
- receipt verification (`verifyReceipt`) — Ed25519 over JCS(document minus
  `signature`), `api_version`, keyset binding;
- body hashes (`checkRequestBodyHash` / `checkResponseBodyHash`).

The provider's own `src/verify.ts` is a thin shim for these, plus the
footer-facing classification. We deliberately do not reimplement the protocol.

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