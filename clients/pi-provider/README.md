# @phala/pi-provider-aci

Vendor-neutral **Pi** provider for [private-ai-gateway] (the ACI protocol),
with **attested TLS (SPKI) pinning** as the security control.

This is the neutral Pi adapter. Branded distributions add their identity on top and
publish their own npm packages:

- [`pi-provider-redpill`](https://www.npmjs.com/package/pi-provider-redpill) — Redpill AI
- [`pi-provider-phala-cloud`](https://www.npmjs.com/package/pi-provider-phala-cloud) — Phala Cloud

Both are thin skins over this package (`createProvider` with an instance-scoped
brand profile). Multiple branded providers can coexist in one process without
sharing provider ids, environment names, config paths, commands, or connection
state.

## Threat model: verified transport and verified responses

Every model request first passes through the verified, SPKI-pinned transport.
That proves the client is connected to the attested workload before prompt
bytes are sent. For every inference, Pi then holds stream completion until the
signed receipt and cited session verify: signature, keyset binding, both body
hashes, serving mode, session integrity, validity window, evidence digest, and
configured session pins. A failed audit terminates the model stream before Pi
can continue its tool loop.

The transport also retains a bounded history of exact wire digests.
`/<provider-id>-receipts` lists it, and `/<provider-id>-receipt [id]` displays or
re-verifies the latest or selected exchange. An exchange outside that history
cannot receive a complete body-hash audit.

## Install

Install the published package:

```bash
pi install npm:@phala/pi-provider-aci
```

Pi owns login, model-catalog persistence, and default-model persistence. For a
branded package, use the provider id shown below:

```text
/login redpill
# or: /login phala
# Phala offers both its account flow and manual API-key entry
# wait for the footer to show aci-verified
/model
# search for the provider, select a model, and press Ctrl+S
```

Pi stores credentials in `~/.pi/agent/auth.json`, dynamic catalogs in
`~/.pi/agent/models-store.json`, and the saved default in
`~/.pi/agent/settings.json`. Environment variables are process inputs and are
not copied into these files. The verified connection and its latest 32
receipt-bearing wire digests are process-local and are cleared when Pi exits;
gateway receipt and session artifacts have separate server-side retention.

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
- Provider-scoped settings, attestation, receipt, and session commands. For the
  neutral package these are `/aci-settings`, `/aci-attestation`, `/aci-receipts`,
  `/aci-receipt`, and `/aci-session`; branded packages replace `aci` with their
  provider id. The receipt commands list history or re-run the complete
  recorded-exchange audit, the session command fetches the public transparency
  artifact and verifies its content address and evidence locally, and the
  attestation command shows the pinned report, keyset digest, binding, keys,
  and expiry.

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
release pipeline publishes them. Until then the provider's attestation command
reports `measurement verified, not pinned`.

The extension registers Pi's native `Provider` and `ApiKeyAuth` interfaces.
Its provider uses Pi's native dynamic-catalog helper, so Pi owns catalog refresh,
offline restoration, persistence, and publication ordering. Pi also owns
credential storage, default-model selection, and environment resolution; the
`openai-completions` adapter receives the connection's scoped fetch through
`StreamOptions.fetch`. A failed or expired connection blocks model traffic,
and a failed receipt audit fails the response stream. Each Pi session gets a
fresh connection and closes it on shutdown.

The same `connectAci()` API works with other Node and Bun SDKs and agent
frameworks; see the [verifier integration examples](../verifier-ts/README.md#runtime-sdk-and-agent-frameworks).
Pi and OpenCode use host-native provider adapters; see the
[coding agent guide](../coding-agents.md). A base URL alone cannot inject ACI's
attested TLS transport, so this release does not claim native support for hosts
without a custom-fetch or provider-plugin extension point.

## Branding / profiles

`createProvider(profile)` registers the provider with a brand identity:

```ts
import { createProvider } from "@phala/pi-provider-aci";
export default createProvider({
  providerId: "my-brand",
  label: "My Brand",
  defaultBaseURL: "https://gateway.example/v1",
  apiKeyEnv: "MY_AI_API_KEY",
  envPrefix: "MY",
  logPrefix: "[my-brand]",
  acceptedComposeHashes: ["<reviewed-sha256-app-compose>"],
  footerKey: "my-brand",
});
```

[private-ai-gateway]: https://github.com/Dstack-TEE/private-ai-gateway
