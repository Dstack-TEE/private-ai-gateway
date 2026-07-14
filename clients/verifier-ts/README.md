# @phala/aci-verifier

A TypeScript verifier for [Attested Confidential Inference
(ACI)](../../spec/aci.md), for the browser and Node 20+. `verifyService(url)`
fetches a service's report with a fresh nonce and returns a full §10.1
transcript — **including the hardware quote**, verified with
[`@phala/dcap-qvl`](https://www.npmjs.com/package/@phala/dcap-qvl) against the
Phala PCCS. Every other check is Web Crypto — Ed25519, X25519, HKDF, AES-GCM,
SHA-256. A prebuilt ESM bundle (`npm run build:bundle`) drops into a
`<script type="module">`.

ACI has no canonical JSON form: artifacts verify as the exact bytes the
service served (spec §3). The only payloads this library constructs are the
two fixed byte templates the spec pins — the attestation statement (§4.2) and
the E2EE AAD (§7.1).

## What it verifies

- **A whole service (§10.1):** `verifyService(url)` fetches the report with a
  fresh nonce and runs the transcript — the quote to the Intel vendor root
  (check 1, via `@phala/dcap-qvl`), the binding chain (checks 2–3), and the
  compose measurement (check 4) when the service publishes `app_compose`.
  Returns `{ verdict, lines, verification }`. `verifyQuote` and
  `verifyComposeMeasurement` are the individual checks.
- **Report binding (§10.1 checks 2–3):** `verifyReportBinding(report, nonce)`
  base64-decodes `workload_keyset_b64`, recomputes the keyset digest over
  those exact bytes, rebuilds the attestation statement for the nonce you
  supplied, checks it hashes to `report_data`, and checks the keyset is not
  expired. The result carries the established keyset (digest, bytes, parsed
  form) for every later check.
- **Receipts (§10.2):** `verifyReceipt(envelope, keyset, establishedDigest)`
  verifies the Ed25519 envelope signature over the decoded `payload_b64`
  bytes under the keyset entry `key_id` names (the attested entry decides the
  algorithm), and that the payload binds to the established keyset digest.
  `checkRequestBodyHash` / `checkResponseBodyHash` cover checks 3–4.
- **Sessions (§9, §10.3):** `computeSessionId` hashes the exact fetched
  session document bytes for comparison against the id a signed receipt
  committed to; `checkSessionEvidence` checks `evidence.data` hashes to
  `evidence.digest`.
- **E2EE v3 (§7):** `openE2eeChannel` seals whole request bodies to the
  attested X25519 key and opens sealed responses, buffered or streamed. It
  refuses a report whose binding did not verify; verify the quote too
  (`verifyService`, or the `aci` CLI) before releasing a prompt to the key.

## What it does not do

- **No custody or TLS-pin check in a plain browser.** §10.1 checks 5–6 are
  verifier-profile / transport territory: key custody (the dstack KMS chain,
  which the `aci` CLI checks) and the observed server-certificate SPKI, which a
  browser cannot see — a pinned channel needs the CLI or the `aci serve` proxy.

Verification failures are reported as `{ ok: false, checks }` — never thrown —
so a caller cannot pass by forgetting a `try/catch`. Errors are thrown only
for malformed input.

## Usage

One call verifies a whole service:

```ts
import { verifyService } from '@phala/aci-verifier';

const { verdict, lines } = await verifyService('https://api.redpill.ai');
console.log(verdict.line); // VERIFIED / PARTIAL / NOT VERIFIED
for (const l of lines) console.log(l.status, l.id, l.title);
```

Or drive the individual checks:

```ts
import {
  verifyReportBinding,
  verifyReceipt,
  checkResponseBodyHash,
  openE2eeChannel,
} from '@phala/aci-verifier';

// Establish the workload identity for a fresh nonce (§10.1 checks 2–3).
const nonce = crypto.randomUUID().replaceAll('-', '');
const report = await (await fetch(`${base}/v1/aci/attestation?nonce=${nonce}`)).json();
const v = await verifyReportBinding(report, nonce);
if (!v.ok) throw new Error('report failed: ' + JSON.stringify(v.checks));

// Verify an inference receipt (§10.2). The envelope comes from
// GET /v1/aci/receipts/{id}, with {id} from the X-Receipt-Id response header.
const result = await verifyReceipt(envelope, v.keyset!, v.workloadKeysetDigest!);
if (!result.ok) throw new Error('receipt failed: ' + JSON.stringify(result.checks));

// Checks 3–4: the bytes you sent and received match what the receipt commits to.
if (!(await checkResponseBodyHash(result.payload!, responseBytes))) {
  throw new Error('response bytes do not match the receipt');
}
```

### E2EE (§7)

Seal the entire request body to the verified workload's attested X25519 key.
The service unseals your exact bytes, so the receipt's `request.received`
hash is reproducible from what you sealed.

```ts
const chan = await openE2eeChannel(v);
const sealed = await chan.seal(JSON.stringify({ model, messages }));
// ...POST sealed.body with sealed.headers to /v1/chat/completions...

const replyBytes = await sealed.open(responseBody);      // buffered reply
// For a streamed (SSE) response, open each event's data payload instead;
// the [DONE] sentinel passes through:
//   const eventJson = await sealed.openStreamEvent(sseEvent.data);
```

## Development

```sh
npm install
npm test      # tsc + node:test, self-consistency suite
npm run build # emit dist/ (ESM + .d.ts)
```
