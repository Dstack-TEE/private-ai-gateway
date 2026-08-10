# private-ai-gateway opencode providers

[opencode](https://opencode.ai) provider plugins for
[private-ai-gateway](https://github.com/Dstack-TEE/private-ai-gateway) (the ACI
protocol), with attested TLS (SPKI) pinning and per-response receipt
verification.

This mirrors `clients/pi-provider` (the pi-coding-agent provider) with the
same architecture: a vendor-neutral core package plus thin branded skins.

- [`@phala/opencode-provider-aci`](packages/opencode-provider-aci) — vendor-neutral core (protocol, pinning, verification)
- [`opencode-provider-phala-cloud`](packages/opencode-provider-phala-cloud) — Phala Cloud branded distribution

## Install (Phala Cloud)

`opencode.json`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "plugin": ["opencode-provider-phala-cloud"]
}
```

Then either log in:

```bash
opencode auth login   # choose Phala Cloud (device flow)
```

or set an API key:

```bash
export PHALA_LLM_API_KEY=...
```

Select a model: `phala/<model-id>`.

Plugin options can be passed via the tuple form:

```json
{
  "plugin": [["opencode-provider-phala-cloud", { "failOpenOnUnpinned": true, "isTeeOnly": true }]]
}
```

## How it works (and how it differs from the pi provider)

The plugin injects a custom `fetch` into the provider options of
`@ai-sdk/openai-compatible` (opencode reads `options.fetch` and routes all
provider traffic through it). That one injection point provides:

- **Attested TLS (SPKI) pinning** — opencode plugins run in opencode's Bun
  runtime, whose fetch accepts `tls: { checkServerIdentity }` per request. At
  first inference use the gateway attestation is validated and the TLS
  connection is pinned to the attested `workload_keyset.tls_public_keys` SPKI.
  **Fail closed by default**: an unpinnable session blocks inference rather
  than silently downgrading to plain CA-TLS; the `failOpenOnUnpinned` option
  opts into the old warning behavior.
- **Full per-response receipt verification** — the injected fetch sees the
  `x-receipt-id` headers AND the raw request/response bytes, so receipts are
  verified completely: Ed25519 signature over JCS AND body hashes against the
  exact bytes exchanged. (The pi provider cannot see response bytes and caps
  at `verified*`; opencode's `verified` is honest.) Statuses:
  `verified` / `verified*` / `routed` / `attested` / `mismatch`.
- Verification is delegated to the repo's reference verifier
  ([`@phala/aci-verifier`](../verifier-ts)) — the same code that ships with
  the gateway, not a private reimplementation.

Status surfacing: opencode has no persistent footer API for plugins, so the
verification status is written to the opencode log after each response AND
shown as a TUI toast (3s for routine statuses, 8s for `mismatch` / `UNPINNED`
/ `PIN REQUIRED` alerts; identical consecutive statuses are not re-toasted).
Structured status is always available via the `phala_verification_status`
tool, and `phala_settings` shows/toggles runtime settings.

## Known gaps vs the pi provider

- **No thinking parameter mapping.** Pi's built-in handler sends
  `enable_thinking` / `reasoning_effort` per model family; opencode's
  `@ai-sdk/openai-compatible` has no equivalent config surface. Models are
  still marked `reasoning: true` and streamed `reasoning_content` surfaces in
  opencode, but no thinking parameter is sent.
- **No settings TUI.** Pi ships a `/aci-settings` SettingsList; opencode
  plugins have no menu surface. The `phala_settings` tool shows and toggles
  runtime settings (pinning, fail-open, receipt auto-verify); persistent
  configuration is via plugin options + `{PREFIX}_*` env vars.
- Model discovery runs at startup using the env API key or the credential
  stored by `opencode auth login` (read from opencode's auth.json). Logging
  in mid-session does not refresh the model list — restart opencode.
- **No proxy support for pinned connections.** The pi provider honors
  HTTP(S)_PROXY via an EnvHttpProxyAgent; Bun's per-request `tls` init does
  not compose with proxies. Pinned inference requires a direct connection.

## Development

```bash
npm install        # requires clients/verifier-ts to be built (npm install && npm run build there)
npm run check      # tsc --noEmit
npm run lint       # oxlint
npm test           # node --test
```
