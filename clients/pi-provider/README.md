# pi-provider (SoT)

Vendor-neutral **Pi** provider for [private-ai-gateway] (the ACI protocol),
with **attested TLS (SPKI) pinning** as the security control.

This directory is the **single source of truth** for the kernel and brand
skins. Users do **not** install from here. Branded release artifacts are
packed into standalone git repos:

| Brand | Artifact repo | Install |
|---|---|---|
| Phala Cloud | [Phala-Network/pi-provider-phala-cloud](https://github.com/Phala-Network/pi-provider-phala-cloud) | `pi install git:github.com/Phala-Network/pi-provider-phala-cloud` |
| Redpill AI | [redpill-ai/pi-provider-redpill](https://github.com/redpill-ai/pi-provider-redpill) | `pi install git:github.com/redpill-ai/pi-provider-redpill` |

Those artifact checkouts also live here as submodules under
`clients/release/`.

`@phala/aci-verifier` is **not** published to npm. Pack embeds a built copy
under `vendor/aci-verifier/` inside each artifact (used for report binding
when establishing the pin).

## Threat model: prevention, not automatic audit

This plugin's job is **prevention**: make sure the request and response are
readable only by the attested workload, via attested TLS (SPKI) pinning.
There is **no field-level E2EE** and **no automatic per-response receipt
verification**.

The gateway still stamps `x-receipt-id` on every reply and serves signed
receipts. This plugin:

- does **not** verify receipts on the hot path;
- **does** capture `x-receipt-id` and expose on-demand inspection:
  `/aci-receipt [id]` and `/aci-session <id>` (raw document summary, no
  signature verification);
- for cryptographic receipt audit, use
  [`@phala/aci-verifier`](../verifier-ts) directly.

## User install

```bash
# Phala Cloud (includes OAuth device login)
pi install git:github.com/Phala-Network/pi-provider-phala-cloud
# or try without persisting:
git clone https://github.com/Phala-Network/pi-provider-phala-cloud
cd pi-provider-phala-cloud && npm install && pi -e .

# Redpill (API key only — no OAuth)
pi install git:github.com/redpill-ai/pi-provider-redpill
```

## Layout (this monorepo)

```
packages/pi-provider-aci/           vendor-neutral kernel (createProvider)
packages/pi-provider-phala-cloud/  Phala brand skin (+ OAuth)
packages/pi-provider-redpill/      Redpill brand skin (no OAuth)
scripts/pack-brand.mjs             pack → standalone artifact root
Makefile                           make pack / make stage
../verifier-ts/                    reference verifier (built into vendor/)
../release/<artifact>/             git submodules → published artifacts
```

## Develop here

```bash
cd clients/verifier-ts && npm ci && npm run build
cd ../pi-provider && npm ci
npm test
npm run check
```

## Pack / publish artifacts

```bash
# build verifier, write both brands, npm install, pi -e . smoke
make pack

# write trees only (CI stage)
make stage

# single brand into a clone you will commit/push
node scripts/pack-brand.mjs --brand phala-cloud --out /path/to/pi-provider-phala-cloud
node scripts/pack-brand.mjs --brand redpill --out /path/to/pi-provider-redpill
```

Pack rules:

- `core/**` and `vendor/**` in the artifact are always overwritten from SoT
- brand `index.ts` is taken from `packages/pi-provider-<brand>/`
- redpill pack **rejects** OAuth markers (API-key-only brand)
- artifact `.npmrc` sets `legacy-peer-deps=true` so pi's
  `npm install --omit=dev` does not materialise `@earendil-works/pi-*` peers

## What the kernel adds

- OpenAI-compatible provider with live model discovery from `/v1/models`
- `is_tee` filtering (configurable)
- Attested TLS (SPKI) pinning, fail-closed by default (no E2EE path)
- `/aci-settings` and `/attestation` (pinned report status)
- On-demand audit trail: `/aci-receipt [id]`, `/aci-session <id>`
  (capture + fetch/summarize only; no signature verification on the hot path)
- No automatic receipt footer / verified* classification

## How the pin is established

`src/aci-client.ts` fetches the attestation report and delegates binding to
`@phala/aci-verifier` (`verifyReportBinding`):
- recomputes the keyset digest from the served keyset,
- checks `report_data` binds our fresh nonce,
- checks `not_after`.

Only a report that passes binding yields an SPKI pin.
`src/tls-pinning.ts` wraps `globalThis.fetch` so pinned hosts fail the TLS
handshake on SPKI mismatch.

## Branding

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
