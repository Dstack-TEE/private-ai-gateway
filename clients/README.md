# ACI clients

Two client surfaces, library and command line:

- [`verifier-ts`](verifier-ts) — `@phala/aci-verifier`, a TypeScript verifier
  for the browser and Node. `verifyService(url)` fetches a service's
  attestation report with a fresh nonce and returns a full §10.1 transcript,
  including the hardware quote (verified with `@phala/dcap-qvl` against the
  Phala PCCS, §10.1 check 1) and the compose measurement (check 4); it also
  covers receipts and body hashes (§10.2), sessions (§9), and the sealed-body
  E2EE channel (§7). Every check but the quote is Web Crypto. Ships an ESM
  bundle for a `<script type="module">`. Key custody (check 5) and the TLS pin
  (check 6) stay out of a plain browser's reach.
- `aci` — the command-line verifier at [`../src/bin/aci`](../src/bin/aci).
  It reuses the reference implementation's verification code and covers the
  full §10.1 check set: `aci verify` (live attestation), `aci audit` (saved
  artifacts), `aci chat` (one inference with receipt verification), and
  `aci serve` (a local OpenAI-compatible proxy that verifies what it
  fronts).

[spec/quickstart.md](../spec/quickstart.md) exercises both against a
live deployment.
