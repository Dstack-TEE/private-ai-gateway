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

## What it verifies

- **A whole service (§9.1):** `verifyService(url)` fetches the report with a
  fresh nonce and runs the transcript — the quote to the Intel vendor root
  (check 1, via `@phala/dcap-qvl`), the binding chain (checks 2–3), and the
  compose measurement (check 4) when the service publishes `app_compose`.
  Returns `{ verdict, lines, verification, composeHash }`. `verifyQuote` and
  `verifyComposeMeasurement` are the individual checks.
- **Production OS allowlist (§1.3):** pass `requireProductionOs: true` to
  require the RTMR3 `os-image-hash` to be in this release's reviewed production
  allowlist. Development and unknown hashes fail closed. This is an appraisal
  step over RTMR3, not a dstack boot verifier. First use a dstack verifier to
  reconstruct MRTD/RTMR0-2 from the same evidence and bind them to
  `os_image_hash`; require the dstack result to report `is_valid: true`.
- **Reviewed release allowlist (§1.3):** pass `acceptedComposeHashes` to accept
  only reviewed `sha256(app_compose)` values measured into RTMR3. Without an
  allowlist the measurement is verified and reported, but the verifier does
  not claim that the release was reviewed.
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

> **Release status:** `@phala/aci-verifier` is not published to npm yet. The
> imports below describe the package's intended public API and work from this
> repository after `npm install && npm run build`. Publishing it is a required
> predecessor to publishing packages that depend on it.

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

### Node SDK and agent frameworks

Node applications can establish an instance-scoped, SPKI-pinned connection
and inject its `fetch` into any HTTP-based OpenAI-compatible SDK. The connection
rejects HTTP, cross-origin requests, expired identities, and TLS peers whose
SPKI is not in the verified workload keyset. It never replaces
`globalThis.fetch`.

```ts
import OpenAI from 'openai';
import { connectAci } from '@phala/aci-verifier/node';

const apiKey = process.env.ACI_API_KEY;
if (!apiKey) throw new Error('ACI_API_KEY is required');

const aci = await connectAci({
  baseURL: 'https://api.example.com/v1',
  apiKey,
  policy: {
    requireProductionOs: true,
    acceptedComposeHashes: ['<reviewed-sha256-app-compose>'],
  },
});

const openai = new OpenAI({
  baseURL: aci.baseURL,
  apiKey,
  fetch: aci.fetch,
});

const response = await openai.chat.completions.create({
  model: 'your-model',
  messages: [{ role: 'user', content: 'Hello' }],
});

await aci.refresh(); // Verify a fresh report and rotate the scoped dispatcher.
await aci.close();
```

`source_provenance.repo_url` and `repo_commit` are published labels, not a
cryptographic release identity. `acceptedComposeHashes` pins the value that is
actually measured into RTMR3. Release automation should publish reviewed
compose hashes alongside each deployment; clients must not learn and trust the
first hash they observe.

#### OpenAI Agents SDK

Use a runner-scoped `OpenAIProvider` so one agent stack owns one verified
connection. `setDefaultOpenAIClient()` also accepts this client, but changes a
process-wide default and is a worse fit when several gateways coexist.

```ts
import { Agent, OpenAIProvider, Runner } from '@openai/agents';

const modelProvider = new OpenAIProvider({
  openAIClient: openai,
  useResponses: false,
});
const runner = new Runner({ modelProvider });
const agent = new Agent({
  name: 'Private agent',
  instructions: 'Be concise.',
  model: 'your-model',
});

const result = await runner.run(agent, 'Summarize this document.');
console.log(result.finalOutput);

await modelProvider.close();
await aci.close();
```

`aci.fetch` protects the HTTP model calls only. Agent tools, MCP calls and
tracing use their own transports and remain separate trust boundaries.

#### Vercel AI SDK

```ts
import { createOpenAICompatible } from '@ai-sdk/openai-compatible';
import { generateText } from 'ai';

const gateway = createOpenAICompatible({
  name: 'aci',
  baseURL: aci.baseURL,
  apiKey,
  fetch: aci.fetch,
});
const result = await generateText({
  model: gateway('your-model'),
  prompt: 'Summarize this document.',
});
```

#### LangChain JS

```ts
import { ChatOpenAI } from '@langchain/openai';

const model = new ChatOpenAI({
  model: 'your-model',
  apiKey,
  configuration: {
    baseURL: aci.baseURL,
    fetch: aci.fetch,
  },
});
const result = await model.invoke('Summarize this document.');
```

These integrations use documented transport hooks in
[OpenAI Node](https://github.com/openai/openai-node/blob/main/src/client.ts),
[OpenAI Agents JS](https://github.com/openai/openai-agents-js/blob/main/packages/agents-openai/src/openaiProvider.ts),
[Vercel AI SDK](https://github.com/vercel/ai/blob/main/packages/openai-compatible/src/openai-compatible-provider.ts),
and [LangChain JS](https://github.com/langchain-ai/langchainjs/blob/main/libs/providers/langchain-openai/src/chat_models/base.ts).

Browsers cannot observe TLS SPKI, and this transport does not cover WebSocket
model calls. For browser clients, WebSocket-only frameworks, or software that
cannot inject a custom `fetch`, run `aci serve` and point the framework at its
local OpenAI-compatible endpoint instead.

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
