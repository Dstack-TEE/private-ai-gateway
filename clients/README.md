# ACI clients

Three client surfaces: a verifier library, a command-line verifier, and a
pi provider extension:

- [`verifier-ts`](verifier-ts) — `@phala/aci-verifier`, a TypeScript verifier
  for the browser and Node. One call, `verifyService(url)`, fetches the
  report with a fresh nonce and returns a full §9.1 transcript. It also
  covers receipts and body hashes (§9.3) and sessions (§8, §9.2). The
  hardware quote is verified with `@phala/dcap-qvl`; every other check is
  Web Crypto. Its Node subpath also exposes `connectAci()`, an instance-scoped
  verified transport that applications can inject into OpenAI Node, OpenAI
  Agents JS, Vercel AI SDK, LangChain JS and other fetch-aware clients without
  replacing global fetch. Ships an ESM bundle for `<script type="module">`.
  Both the Node transport and CLI can pin reviewed RTMR3-bound compose hashes;
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
- [`pi-provider`](pi-provider) — a [pi](https://pi.dev/) provider
  extension that turns the gateway (or any ACI service) into a first-class
  chat provider in pi's model picker, with **attested TLS (SPKI) pinning** as
  the security control (always fail closed). The npm workspaces monorepo
  ships the vendor-neutral
  [`@phala/pi-provider-aci`](pi-provider/packages/pi-provider-aci) core plus
  thin branded distributions,
  [`pi-provider-redpill`](pi-provider/packages/pi-provider-redpill) and
  [`pi-provider-phala-cloud`](pi-provider/packages/pi-provider-phala-cloud).
  The core provides live model discovery from `/v1/models`, `is_tee`
  filtering, and injects the shared Node verified transport through Pi's
  provider-scoped fetch hook. No build step — pi loads `.ts` directly.
  Per-response receipt verification is intentionally not part of this
  extension (prevention, not audit); use the `verifier-ts` library above if
  you want to audit receipts. On request the plugin can show the latest
  receipt and attested session as an audit trail (`/aci-receipt`, `/aci-session`).
  See [`pi-provider/README.md`](pi-provider/README.md)
  for install and use.

Coding agents that cannot inject a custom HTTP transport use `aci serve` as a
single local verification boundary. See [`coding-agents.md`](coding-agents.md)
for Codex, Claude Code, OpenCode, Aider and Gemini CLI compatibility.

[docs/quickstart.md](../docs/quickstart.md) exercises both verifier
surfaces against a live deployment. The `pi-provider` extension is loaded
with pi's `-e` flag pointing at one of the package directories (e.g.
`pi -e clients/pi-provider/packages/pi-provider-aci`), or installed as a
pi package once published; see its README for the precise invocation.
