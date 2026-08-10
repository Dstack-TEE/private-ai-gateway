# ACI clients

Four client surfaces: a verifier library, a command-line verifier, and two
agent provider plugins (pi and opencode):

- [`verifier-ts`](verifier-ts) — `@phala/aci-verifier`, a TypeScript verifier
  for the browser and Node. One call, `verifyService(url)`, fetches the
  report with a fresh nonce and returns a full §9.1 transcript. It also
  covers receipts and body hashes (§9.3) and sessions (§8, §9.2). The
  hardware quote is verified with `@phala/dcap-qvl`; every other check is
  Web Crypto. Ships an ESM bundle for `<script type="module">`. Key custody
  (§9.1 check 5) is an honest skip in both verifiers; the channel check (6)
  needs an observed SPKI (or the `aci` CLI / `aci serve` proxy for a pinned
  channel) — this client ships no E2EE (§6) this round.
- `aci` — the command-line verifier at [`../src/bin/aci`](../src/bin/aci).
  It reuses the reference implementation's verification code:
  `aci verify` (live attestation), `aci audit` (saved artifacts),
  `aci sessions` (the §9.2 audit of the service's current attested
  sessions, with a `--require-claim` claims policy), `aci send` (one
  inference with receipt verification), and `aci serve` (a local verifying
  proxy: forwards any endpoint over the pinned channel, records each
  exchange's digests for on-demand receipt verification, and pins sessions
  per §5.3 — a fixed `--session` list, or a `--require-claim` policy that
  derives the accepted set and refreshes it when the service refuses a
  superseded pin).
- [`pi-provider`](pi-provider) — SoT for the [pi](https://pi.dev/) provider
  extension (kernel + brand skins). Security control is **attested TLS
  (SPKI) pinning** (fail-closed); there is no field-level E2EE and no
  automatic per-response receipt verification. On request the plugin can
  show the latest receipt / attested session as an audit trail
  (`/aci-receipt`, `/aci-session`) without verifying signatures — use
  `verifier-ts` for cryptographic audit. Users install **release artifacts**,
  not this tree:
  `pi install git:github.com/Phala-Network/pi-provider-phala-cloud` or
  `pi install git:github.com/redpill-ai/pi-provider-redpill`
  (also `npm install && pi -e .` inside a clone). Artifacts embed a built
  `@phala/aci-verifier` under `vendor/`. Submodule checkouts:
  [`release/`](release/). Pack: `make -C pi-provider pack`.
  See [`pi-provider/README.md`](pi-provider/README.md).
- [`opencode-provider`](opencode-provider) — an [opencode](https://opencode.ai)
  provider plugin that wires the gateway into opencode via a custom `fetch`
  injected into `@ai-sdk/openai-compatible` provider options: attested TLS
  (SPKI) pinning per connection (Bun `tls.checkServerIdentity`, fail closed),
  plus full receipt verification — the injected fetch sees the exact
  request/response bytes, so body hashes are checked honestly (`verified`,
  not pi's inspection-only `/aci-receipt`). The npm workspaces monorepo ships
  the vendor-neutral
  [`@phala/opencode-provider-aci`](opencode-provider/packages/opencode-provider-aci)
  core plus the branded
  [`opencode-provider-phala-cloud`](opencode-provider/packages/opencode-provider-phala-cloud)
  distribution. See [`opencode-provider/README.md`](opencode-provider/README.md)
  for install and use.

[docs/quickstart.md](../docs/quickstart.md) exercises both verifier
surfaces against a live deployment. For pi, install a brand artifact or load
a local pack with `npm install && pi -e .`. SoT development can still use
workspace paths under `pi-provider/packages/`.
