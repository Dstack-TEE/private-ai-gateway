# ACI clients

Two client surfaces, library and command line:

The TypeScript package is currently private and built from source. The `aci`
CLI ships as checksum-verified Linux and macOS release binaries for x86-64 and
ARM64. See the [`aci` CLI reference](../src/bin/aci/README.md) for installation.

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
  sessions, with a `--require-claim` claims policy), `aci curl` (run the
  installed curl over the attested SPKI-pinned channel), `aci send` (one
  inference with receipt verification), and `aci serve` (a local verifying
  proxy: forwards any endpoint over the pinned channel, records each
  exchange's digests for on-demand receipt verification, and pins sessions
  per §5.3 — a fixed `--session` list, or a `--require-claim` policy that
  derives the accepted set and refreshes it when the service refuses a
  superseded pin).

[docs/quickstart.md](../docs/quickstart.md) exercises both against a
live deployment.
