# ACI clients

Start with the [client product architecture](architecture.md) for the product
goal, shared trust contract, component ownership, and remaining release work.
Framework-specific coding-agent configuration is in
[`coding-agents.md`](coding-agents.md).

Four client layers cover verification, framework-neutral provider behavior,
and native coding-agent integrations:

- [`verifier-ts`](verifier-ts) — `@phala/aci-verifier`, a TypeScript verifier
  for the browser, Node, and Bun. One call, `verifyService(url)`, fetches the
  report with a fresh nonce and returns a full §9.1 transcript. It also
  covers receipts and body hashes (§9.3) and sessions (§8, §9.2). The
  hardware quote is verified with `@phala/dcap-qvl`; every other check is
  Web Crypto. Its runtime subpath also exposes `connectAci()`, an
  instance-scoped verified transport with tested Node and Bun adapters that
  applications can inject into OpenAI Node, OpenAI Agents JS, Vercel AI SDK,
  LangChain JS, OpenCode and other fetch-aware clients. Ships an ESM bundle
  for `<script type="module">`.
  Both runtime transports and the CLI can pin reviewed RTMR3-bound compose hashes;
  self-declared repository and commit fields are informational labels.
  Key custody (§9.1 check 5) is an honest skip in both verifiers; the channel
  check (6) needs an observed SPKI (or the `aci` CLI / `aci serve` proxy for a
  pinned channel) — this client ships no E2EE (§6) this round.
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
- [`provider`](provider) — `@phala/aci-provider`, the framework-neutral ACI
  provider. It owns verified connection lifecycle, live model discovery,
  TEE-only and allowlist filtering, model capability mapping, bounded receipt
  history, and response-completion receipt verification. Pi and OpenCode use
  this package rather than implementing those behaviors separately.
- [`pi-provider`](pi-provider) — a [pi](https://pi.dev/) provider
  extension that turns the gateway (or any ACI service) into a first-class
  chat provider in pi's model picker, with **attested TLS (SPKI) pinning** as
  the security control (always fail closed). The npm workspaces monorepo
  ships the vendor-neutral
  [`@phala/pi-provider-aci`](pi-provider/packages/pi-provider-aci) adapter plus
  thin branded distributions,
  [`pi-provider-redpill`](pi-provider/packages/pi-provider-redpill) and
  [`pi-provider-phala-cloud`](pi-provider/packages/pi-provider-phala-cloud).
  It adds Pi configuration, model types, settings, commands, credentials, and
  footer state around the shared provider.
  The transport records bounded wire digests while streaming, and
  `/aci-receipt` verifies the signed receipt plus cited session on demand.
  `/aci-session` remains available for direct session inspection.
  See [`pi-provider/README.md`](pi-provider/README.md)
  for install and use. The coordinated npm release process is documented in
  [`releasing.md`](releasing.md).
- [`opencode-provider`](opencode-provider) — the native OpenCode v1 server
  plugin. `@phala/opencode-provider-aci` maps the shared provider into
  OpenCode's config, auth, model, reasoning, and lifecycle hooks;
  `opencode-provider-redpill` supplies the RedPill profile. It verifies every
  receipt before the response stream completes, so a failed audit stops the
  generation/tool loop.

SDKs that accept a function can use `connectAci().fetch` directly. Native Pi
and OpenCode integrations use the provider packages above. Agents that only
accept a base URL use `aci serve` as a local verification boundary.

[docs/quickstart.md](../docs/quickstart.md) exercises both verifier
surfaces against a live deployment. The `pi-provider` extension is loaded
with pi's `-e` flag pointing at one of the package directories (e.g.
`pi -e clients/pi-provider/packages/pi-provider-aci`), or installed from npm; see its README for the invocation.
