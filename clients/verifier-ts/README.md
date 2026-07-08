# @dstack/aci-verifier

A zero-dependency TypeScript verifier for [Attested Confidential Inference
(ACI)](../../spec/aci.md). It implements **Level 1 — receipt verification**
(spec §10.2) and the cryptographic-binding subset of **Level 2** (§10.1 checks
2–6), using only the Web Crypto API (Ed25519, SHA-256) plus a small built-in
JCS canonicalizer. The same code runs in a browser and in Node 20+; nothing in
`src/` uses a Node-only API.

Every construction is checked byte-for-byte against
[spec/test-vectors.md](../../spec/test-vectors.md) — `workload_id`, the keyset
digest, `report_data`, keyset endorsement and revocation signatures,
`session_id`, the receipt canonical bytes and signature, and the E2EE AAD
strings.

## What it verifies

- **Receipts (Level 1, §10.2 checks 1–2):** `verifyReceipt(receipt, keyset)`
  checks the receipt signature under a key in the established keyset's
  `receipt_signing_keys`, that `signature.algo` matches that key, and that the
  receipt's `workload_id` / `workload_keyset_digest` match the established
  keyset. Body-hash helpers (`checkRequestBodyHash`, `checkResponseWireHash`,
  `checkResponseCleartextHash`) cover checks 3–4.
- **Report binding (Level 2, §10.1 checks 2–6, no hardware quote):**
  `verifyReportBinding(report, nonce)` recomputes `workload_id`, the keyset
  digest, and `report_data` for the supplied nonce, verifies the keyset
  endorsement under the identity key, and checks epoch freshness.
- **Digest & canonicalization primitives:** JCS canonicalization,
  `computeWorkloadId`, `computeKeysetDigest`, `computeReportData`,
  `computeSessionId`, `receiptSigningBytes`, and the endorsement/revocation
  payload builders.
- **E2EE AAD builders (§7.3):** `requestAad` / `responseAad` for clients that
  encrypt request/response fields.

## What it does not do

- **No hardware quote verification.** §10.1 check 1 (the TDX/SEV-SNP quote
  verifies to the vendor root) and the "hardware evidence binds `report_data`"
  half of check 4 are verifier-profile / Level 2 territory and need primitives
  outside the Web Crypto API. `verifyReportBinding` proves only the
  cryptographic *binding* of the report; compose it with a quote verifier and
  the custody / provenance / channel checks (§10.1 checks 1, 7–10) for full
  Level 2.
- **No `ecdsa-secp256k1`.** The curve is not in the Web Crypto API, so any
  secp256k1 signature or identity key raises `UnsupportedAlgorithmError` — verify
  those against the reference implementation or a Level 2 profile.
- **No upstream/session deep audit** (Level 3, §10.3) beyond `computeSessionId`,
  which lets you recompute and compare a session id a receipt committed to.

Verification failures are reported as `{ ok: false, checks }` — never thrown —
so a caller cannot pass by forgetting a `try/catch`. Errors are thrown only for
malformed input or an out-of-scope algorithm.

## Usage

```ts
import { verifyReceipt, checkResponseWireHash } from '@dstack/aci-verifier';

// `keyset` is a WorkloadKeyset you already trust — from a Level 2 report
// verification, or published by a party you trust. `receipt` and `responseBytes`
// come from the inference response you received.
const result = await verifyReceipt(receipt, keyset);
if (!result.ok) {
  throw new Error('receipt failed: ' + JSON.stringify(result.checks));
}

// §10.2 checks 3–4: the response bytes you saw match what the receipt commits to.
if (!(await checkResponseWireHash(receipt, responseBytes))) {
  throw new Error('response bytes do not match the receipt');
}
```

### E2EE (§7)

Encrypt request fields to a *verified* workload. `openE2eeChannel` refuses
unless the report passed `verifyReportBinding`, so you can only encrypt to an
attested, endorsed key (X25519 suite; secp256k1 is a separate extension).

```ts
import { verifyReportBinding, openE2eeChannel } from '@dstack/aci-verifier';

const v = await verifyReportBinding(report, attestationNonce);
if (!v.ok) throw new Error('workload failed verification');

const chan = await openE2eeChannel(report, v);
const { body, headers } = await chan.seal({ model, messages }); // encrypts content, sets X-E2EE-*
// ...POST body + headers to /v1/chat/completions...
const reply = await chan.open(responseJson);                    // buffered reply
// For a streamed (SSE) response, decrypt each event's chunk instead:
//   const chunk = await chan.openChunk(JSON.parse(sseEvent.data));
```

`seal` also covers `/v1/completions` (`prompt`) and `/v1/embeddings` (`input`);
`open` covers `message.audio.data`, completion `text`, and embedding vectors.

## Development

```sh
npm install
npm test     # tsc + node:test against every spec vector
npm run build # emit dist/ (ESM + .d.ts)
```
