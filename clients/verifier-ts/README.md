# @phala/aci-verifier

A TypeScript verifier for [Attested Confidential Inference
(ACI)](../../spec/aci.md), for the browser and Node 20+. `verifyService(url)`
fetches a service's report with a fresh nonce and returns a full §9.1
transcript — **including the hardware quote**, verified with
[`@phala/dcap-qvl`](https://www.npmjs.com/package/@phala/dcap-qvl) against the
Phala PCCS. Every other check is Web Crypto — Ed25519, X25519, HKDF, AES-GCM,
SHA-256. A prebuilt ESM bundle (`npm run build:bundle`) drops into a
`<script type="module">`.

ACI documents verify over their JCS form (spec Appendix A), so this library
canonicalizes whatever it parsed and hashes foreign bytes (HTTP bodies,
evidence) exactly as observed.

The package is currently marked `private` and is built from this repository;
it is not a supported npm release yet.

## What it verifies

- **A whole service (§9.1):** `verifyService(url)` fetches the report with a
  fresh nonce and runs the transcript — the quote to the Intel vendor root
  (check 1, via `@phala/dcap-qvl`), the binding chain (checks 2–3), and the
  compose measurement (check 4) when the service publishes `app_compose`.
  Returns `{ verdict, lines, verification }`. `verifyQuote` and
  `verifyComposeMeasurement` are the individual checks.
- **Production OS allowlist (§1.3):** pass `requireProductionOs: true` to
  require the RTMR3 `os-image-hash` to be in this release's reviewed production
  allowlist. Development and unknown hashes fail closed. This is an appraisal
  step over RTMR3, not a dstack boot verifier. First use a dstack verifier to
  reconstruct MRTD/RTMR0-2 from the same evidence and bind them to
  `os_image_hash`; require the dstack result to report `is_valid: true`.
- **Report binding (§9.1 checks 2–3):** `verifyReportBinding(report, nonce)`
  recomputes the keyset digest over the served `workload_keyset` object's
  JCS form, rebuilds the attestation statement for the nonce you
  supplied, checks it hashes to `report_data`, and checks the keyset is not
  expired. The result carries the established keyset (digest, parsed
  form) for every later check.
- **Receipts (§9.3):** `verifyReceipt(document, keyset, establishedDigest)`
  verifies the Ed25519 signature over the JCS form of the document minus
  its `signature` member, under the keyset entry `key_id` names, and that
  the document binds to the established keyset digest.
  `checkRequestBodyHash` / `checkResponseBodyHash` cover checks 3–4.
- **Sessions (§8, §9.2):** `computeSessionId` hashes the JCS form of the
  parsed session document for comparison against the id a signed receipt
  committed to; `checkSessionEvidence` checks `evidence.data` hashes to
  `evidence.digest`.
- **Aggregator checks (§9.3(5)-(6)):** `receiptTranscript(..., upstream)`
  adds upstream-1 (the serving upstream was verified and cites a session) and
  upstream-2 (the cited session hashes to its id, covers `served_at`, carries matching
  evidence, and is one you pinned with `provider.aci_session_ids`). Pass
  `{ session, pinnedSessions, requiresVerified }`; anything absent is a skip
  with its reason, never a pass.
- **E2EE key verification, not an E2EE request builder.** Report verification
  establishes the quote-bound `e2ee_public_keys`, including the suites in the
  [E2EE v2 compatibility protocol](../../spec/e2ee-v2.md). This package does
  not yet encrypt or decrypt content fields. Callers can implement that
  field-level wire contract or use a separate v2 client.

## What it does not do

- **No dstack boot-measurement reconstruction.** Quote verification
  authenticates the quote's RTMR fields, and this package replays RTMR3. It
  does not reconstruct MRTD/RTMR0-2 from a dstack OS image. A
  `requireProductionOs` pass is meaningful only together with a dstack
  verifier result for the same quote, event log, and VM configuration. See
  [How the OS image is classified](../../docs/providers/phala-direct/verification.md#how-the-os-image-is-classified).
- **No custody check.** §9.1 check 5 (the dstack KMS chain) is not
  implemented in either in-tree verifier; both report an honest skip
  (conformance gaps item 1).
- **No TLS observation in a browser.** A browser cannot see the server
  certificate, so id-6 needs the SPKI your own TLS stack observed (the
  `channel` option) — or the `aci` CLI / `aci serve` proxy, which can; with
  neither, a live run fails id-6 (§1.1).
- **No deep audit of upstream evidence (§9.2(4)).** `checkSessionEvidence`
  proves the cited session's `evidence.data` hashes to `evidence.digest` and
  stops there; neither in-tree verifier appraises the evidence itself
  (conformance gaps item 5).
- **Ed25519 receipts only.** A receipt keyed to any other algorithm is
  reported as a failed signature check, not verified.

Verification failures are reported as `{ ok: false, checks }` — never thrown —
so a caller cannot pass by forgetting a `try/catch`. Errors are thrown only
for malformed input.

## Usage

One call runs the ACI checks and OS-hash appraisal. This example assumes a
dstack verifier has already returned `is_valid: true` for the same evidence:

```ts
import { verifyService } from '@phala/aci-verifier';

const { verdict, lines } = await verifyService('https://tee.redpill.ai', {
  requireProductionOs: true,
});
console.log(verdict.line); // VERIFIED / PARTIAL / NOT VERIFIED
for (const l of lines) console.log(l.status, l.id, l.title);
```

Or drive the individual checks:

```ts
import {
  verifyReportBinding,
  verifyReceipt,
  checkResponseBodyHash,
} from '@phala/aci-verifier';

// Establish the workload identity for a fresh nonce (§9.1 checks 2–3).
const nonce = Array.from(crypto.getRandomValues(new Uint8Array(32)), (b) => b.toString(16).padStart(2, '0')).join('');
const report = await (await fetch(`${base}/v1/aci/attestation?nonce=${nonce}`)).json();
const v = await verifyReportBinding(report, nonce);
if (!v.ok) throw new Error('report failed: ' + JSON.stringify(v.checks));

// Check the response against its receipt (§9.3). The receipt document
// comes from GET /v1/aci/receipts/{id}, {id} from the X-Receipt-Id header.
const result = await verifyReceipt(receipt, v.keyset!, v.workloadKeysetDigest!);
if (!result.ok) throw new Error('receipt failed: ' + JSON.stringify(result.checks));

// Checks 3–4: the bytes you sent and received match what the receipt commits to.
if (!(await checkResponseBodyHash(result.payload!, responseBytes))) {
  throw new Error('response bytes do not match the receipt');
}
```

### E2EE

E2EE v2 is a supported field-level compatibility extension, specified
separately from core ACI in [the v2 protocol](../../spec/e2ee-v2.md). It is
supported through at least February 10, 2027 and is planned to be replaced by
E2EE v3. This verifier establishes the attested E2EE keys but does not construct
encrypted requests or decrypt responses. Use a v2-capable client for those
operations. Without one, a bound channel needs the caller-observed TLS SPKI
(`channel`) or the `aci` CLI / `aci serve` proxy.

## Development

```sh
npm install
npm test      # tsc + node:test; test/vectors.test.ts pins every
              # construction against the ACI and E2EE v2 vector documents
npm run build # emit dist/ (ESM + .d.ts)
```
