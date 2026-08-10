# @phala/opencode-provider-aci

Vendor-neutral **opencode** provider plugin for [private-ai-gateway] (the ACI
protocol), with attested TLS (SPKI) pinning and per-response receipt
verification.

This is the neutral core. Branded distributions add their identity on top and
publish their own npm packages:

- [`opencode-provider-phala-cloud`](https://www.npmjs.com/package/opencode-provider-phala-cloud) — Phala Cloud

Branded shells are thin skins over this package (`createProvider` with a brand
profile, imported from `@phala/opencode-provider-aci/core`) — the underlying
protocol code lives here once. The package's default export is the neutral
plugin itself; opencode's loader treats every runtime export as a plugin, so
the library surface is kept on the `/core` subpath.

## Install (core)

`opencode.json`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "plugin": ["@phala/opencode-provider-aci"]
}
```

```bash
export ACI_BASE_URL=https://<your-gateway>/v1   # your private-ai-gateway endpoint
export ACI_LLM_API_KEY=...
```

## What it adds

- OpenAI-compatible provider (`aci`) with live model discovery from
  `/v1/models` (no hardcoded catalog).
- `is_tee` filtering — only confidentially-served models are registered by default.
- **Attested TLS (SPKI) pinning** — at first inference use the gateway
  attestation is validated and the TLS connection is pinned to the attested
  `workload_keyset.tls_public_keys` SPKI (via Bun's per-request
  `tls.checkServerIdentity`). **Fail closed by default**: with `pinning`
  enabled (the default) an unpinnable session blocks inference rather than
  silently downgrading to plain CA-TLS; the `failOpenOnUnpinned` plugin option
  opts into running unpinned with a warning.
- **Full per-response receipt verification** — the injected fetch captures the
  exact request/response bytes, so `verified` means the receipt signature
  validated AND the body hashes were checked against the bytes exchanged.
  `mismatch` means a signature FAILED or the keyset did not match. Backed by
  the repo's reference verifier ([`@phala/aci-verifier`](../../verifier-ts)).
- `aci_verification_status` tool — structured pin/receipt/attestation status.

## Configuration

Layers, lowest to highest precedence:

1. defaults (`isTeeOnly: true`, `autoFetchReceipt: true`, `pinning: true`, fail-closed)
2. plugin options — `{"plugin": [["@phala/opencode-provider-aci", {...}]]}` with
   flat keys: `baseUrl`, `isTeeOnly`, `allowlist`, `autoFetchReceipt`,
   `requireAttestationMatch`, `failOpenOnUnpinned`, `pinning`
3. env — `{PREFIX}_BASE_URL`, `{PREFIX}_IS_TEE_ONLY`, `{PREFIX}_MODEL_ALLOWLIST`,
   `{PREFIX}_AUTO_FETCH_RECEIPT`, `{PREFIX}_REQUIRE_ATTESTATION_MATCH`,
   `{PREFIX}_FAIL_OPEN_ON_UNPINNED`, `{PREFIX}_PINNING` (`ACI_` prefix for the core)
4. runtime — `createProvider(profile, overrides)` patch

User-declared `config.provider.aci` entries in opencode.json are respected:
standard options (`baseURL`, `apiKey`, `headers`) and models merge over the
plugin's defaults. Only `options.fetch` is not overridable — it is the
security boundary (pin enforcement + receipt capture).

## How verification is wired

`src/aci-client.ts` fetches the ACI artifacts and delegates all cryptographic
verification to `@phala/aci-verifier`:

- report binding (`verifyReportBinding`) — keyset digest, `report_data`,
  `not_after`;
- receipt verification (`verifyReceipt`) — Ed25519 over JCS(document minus
  `signature`), `api_version`, keyset binding;
- body hashes (`checkRequestBodyHash` / `checkResponseBodyHash`).

`src/pinned-fetch.ts` enforces the pin and captures the exchange bytes; the
provider's own `src/verify.ts` is a thin re-export shim. We deliberately do
not reimplement the protocol.

## Branding / profiles

`createProvider(profile)` registers the provider with a brand identity:

```ts
import { createProvider } from "@phala/opencode-provider-aci/core";
export default createProvider({
  providerId: "my-brand",
  label: "My Brand",
  defaultBaseUrl: "https://gateway.example/v1",
  apiKeyEnv: "MY_LLM_API_KEY",
  envPrefix: "MY",
  logPrefix: "[my-brand]",
  fallbackModels: [...],
  // optional RFC 8628 device-flow login for `opencode auth login`:
  oauth: { name: "My Brand", startDeviceFlow, pollDeviceFlow },
});
```

[private-ai-gateway]: https://github.com/Dstack-TEE/private-ai-gateway
