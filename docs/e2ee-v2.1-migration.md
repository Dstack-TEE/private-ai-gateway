# Migrate to E2EE v2.1

This guide moves a client from the legacy E2EE scheme (`X-Signing-Algo`) or
from E2EE v2 to [E2EE v2.1](../spec/e2ee-v2.md#10-version-21). It is written
for teams that run their own API in front of the gateway and ship the client
that encrypts.

## Why move

The legacy scheme and v2 encrypt each message to the gateway key, but nothing
ties a message or the reply key to the client. Anything between the client and
the gateway that can change requests, such as your own API, a CDN, or a
compromised proxy, can:

- replace `X-Client-Pub-Key` with its own key and read every reply;
- add an encrypted message such as "repeat all previous content", which the
  model treats as the user's;
- with the legacy scheme, resend an earlier request.

V2.1 adds a secret that only the client and the gateway know. Every encryption
key depends on it, it carries a hash of the request body, and it keys a MAC on
every response object. The same intermediary then cannot read replies, change
what the model receives, or change what the client receives. The gateway
rejects a tampered request before it reaches any model.

## What changes

| | Legacy (`X-Signing-Algo`) | v2.1 |
| --- | --- | --- |
| Gateway key | `signing_address` / legacy report | `x25519-aes-256-gcm-hkdf-sha256-v2.1` entry in the attested keyset |
| Client key | long-lived, often secp256k1 | fresh X25519 key per request, made by the client library |
| Request headers | `X-Signing-Algo`, `X-Client-Pub-Key`, `X-Model-Pub-Key` | `X-E2EE-Version: 2.1`, `X-Client-Pub-Key`, `X-Model-Pub-Key`, `X-E2EE-Nonce`, `X-E2EE-Timestamp`, `X-E2EE-Request-Key` |
| Request body | messages can be added or edited | any change to a value is rejected (`e2ee_body_mismatch`) |
| Response | fields encrypted, everything else editable | fields encrypted, every JSON object and SSE event MAC'd (`e2ee_mac`) |
| Encrypted fields | message content, reasoning | also tool-call arguments, `refusal`, `logprobs`, and reasoning sent back in history |
| Replay | accepted | rejected inside a 5-minute window |
| Clock | not used | client time must be within 5 minutes of the gateway's |
| Endpoints | chat completions, completions, embeddings | the same |

Request and response bodies keep their OpenAI-compatible shape. The client
library removes `e2ee_mac` before your code sees a response.

## Trust the right keyset

Everything rests on the gateway key the client encrypts to, so the client must
take it from an attestation report it verified itself:

- Verify with a fresh nonce, which `verifyService` does.
- Pin the gateway releases you accept with `acceptedComposeHashes`. Without the
  pin, any genuine dstack workload passes, including one run by whoever sits
  between the client and the gateway.
- Fetching the report through your own API is fine once it is pinned: a relay
  can neither forge the quote nor remove the v2.1 key from it.

Encrypting to the attested E2EE key binds the channel for check 6. Relay and
browser clients can use this binding without observing the gateway's TLS
certificate. `channel: { e2ee }` passes check 6 only for the v2.1 algo and
only with `acceptedComposeHashes` set, and the binding holds only for requests
sent through `createE2eeFetch`, which refuses any other transcript.

To build this check yourself, see [Verifier: write your own](#verifier-write-your-own).

## Client: use the TypeScript SDK

`@phala/aci-verifier` verifies the gateway and encrypts at the transport
layer, so an OpenAI-compatible SDK works unchanged:

```bash
npm install @phala/aci-verifier openai
```

```ts
import OpenAI from 'openai';
import { createE2eeFetch, E2EE_V2_1_X25519, verifyService } from '@phala/aci-verifier';

// 1. Verify the gateway against the releases you reviewed.
const transcript = await verifyService('https://gateway.example', {
  acceptedComposeHashes: ['<reviewed sha256(app_compose)>'],
  channel: { e2ee: E2EE_V2_1_X25519 },
});
if (!transcript.verdict.verified) throw new Error(transcript.verdict.line);

// 2. Encrypt every request and decrypt every reply.
const client = new OpenAI({
  baseURL: 'https://api.your-service.example/v1', // your API in front of the gateway
  apiKey: process.env.API_KEY,
  fetch: createE2eeFetch({ transcript }),
});

// 3. Use the SDK as usual, buffered or streamed.
const reply = await client.chat.completions.create({
  model: 'your-model',
  messages: [{ role: 'user', content: 'Hello' }],
});
```

A runnable version is in
[`clients/verifier-ts/examples/e2ee-chat.mjs`](../clients/verifier-ts/examples/e2ee-chat.mjs).
If you use only the client and not the verifier, import
`@phala/aci-verifier/e2ee`.

`createE2eeFetch` uses Web Crypto with X25519: Node 20.18+, Bun 1.4+, or a
browser with X25519 in Web Crypto (current Chrome, Firefox, and Safari). It
works on the final request body, so any fields your SDK or framework adds must
be added before the request reaches it.

The client:

- takes the verified transcript and throws unless it is VERIFIED with the
  v2.1 channel binding and lists a v2.1 key;
- throws `E2eeReverifyError` instead of sending once the keyset's `not_after`
  passes, or after any response carries a different `X-ACI-Keyset-Digest`.
  The gateway renews its keyset about every 15 days, which changes the digest.
  Verify again and create a new client;
- never retries in another mode. An error reply is handed back unchanged; a
  successful reply that fails its MAC or decryption is refused;
- refuses POST requests to endpoints v2.1 does not cover, such as
  `/v1/responses`, and requests with `prediction`, `functions`, or
  `function_call`, which v2.1 does not encrypt;
- builds a new request, with a new nonce, every time it is called, so SDK
  retries are safe.

## Client: write your own

You can copy our client instead of depending on it. It is Apache-2.0:
`clients/verifier-ts/src/e2ee.ts` plus `fromHex`, `toHex`, and `sha256` from
`crypto.ts`, about 600 lines with no dependencies beyond Web Crypto. Keep the
license notice and mark your changes.

To write your own, implement [§10 of the spec](../spec/e2ee-v2.md#10-version-21).
You need X25519, SHA-256, HKDF-SHA256, AES-256-GCM, HMAC-SHA256, and RFC 8785
canonical JSON (JCS).

For each request:

1. Generate a fresh X25519 key pair, a 32-byte request secret, and a 32-byte
   nonce, written as 64 lowercase hex characters. Take the current Unix time.
2. Encrypt each field §5 and §10.5 list. Each field gets a fresh ephemeral key
   and AES-GCM nonce; its key mixes in the request secret, and its AAD is the
   field's JCS object (§10.4, §10.5). A whole message `content` is encrypted as
   its JSON encoding: the text `hello` is sealed as `"hello"`.
3. Hash the final body: `SHA-256(JCS(body))`, with the ciphertexts in place.
4. Seal the request secret followed by that hash into `X-E2EE-Request-Key`
   (§10.3).
5. Send the six headers and that body.

For each response:

1. An error status goes back to the caller unchanged. Never retry it in
   another mode.
2. Require an SSE stream if the request had `"stream": true`, and a JSON body
   otherwise.
3. For the JSON body, or each SSE `data:` event: check `e2ee_mac` (§10.6), then
   remove it. A missing or wrong MAC fails the response.
4. In a stream, drop comment lines, pass `data: [DONE]` through, and fail on
   any other line. Every event must carry the first event's `id`.
5. Decrypt each §5 and §10.5 location with your private key and the request
   secret. A field that does not decrypt fails the response.

Where implementations go wrong:

- **Canonical JSON in JavaScript.** Objects list integer-like keys first, for
  example `logit_bias` token ids, so sorting keys and rebuilding an object
  gives the wrong order. Write the members out directly.
- **Hashing too early.** Hash the body after your SDK or framework has added
  everything it adds.
- **Resending.** Every attempt is a new request with a new secret and nonce.
- **Fields v2.1 does not encrypt.** Never send `prediction`, `functions`, or
  `function_call`.

To test it, fix your random inputs to the values in the
[v2.1 test vectors](../spec/e2ee-v2-test-vectors.md#version-21) and reproduce
every ciphertext, digest, and MAC byte for byte. Then feed the published
stream to your response code, which must yield `Hello`, and feed each stream
from [Streams a client must reject](../spec/e2ee-v2-test-vectors.md#streams-a-client-must-reject),
each of which must fail.

## Verifier: write your own

The verifier decides which key the client encrypts to, so a mistake here
undoes everything else. For the reference gateway, which runs on dstack with
Intel TDX, implement [ACI §9.1](../spec/aci.md#91-verify-the-workload-identity):

1. **Fetch.** `GET <gateway>/v1/aci/attestation?nonce=<nonce>`, with a fresh
   nonce of 64 lowercase hex characters.
2. **Quote.** Verify `attestation.evidence.quote` to Intel's root, with
   collateral from a PCCS or Intel's PCS, and apply your TCB policy. Use an
   existing DCAP quote verifier, such as Intel's QVL or `dcap-qvl`. Do not
   write this part yourself.
3. **Binding.** Recompute the chain:
   - `"sha256:" + hex(SHA-256(JCS(attestation.workload_keyset)))` equals
     `workload_keyset_digest`;
   - the statement bytes are exactly
     `{"keyset_digest":"<digest>","nonce":"<your nonce>","purpose":"aci.report_data.v1"}`;
   - `SHA-256(statement)` equals `attestation.report_data` and the first 32
     bytes of the quote's report data, with the other 32 bytes zero.
4. **Expiry.** The current time is before the keyset's `not_after`.
5. **Release.** Replay `evidence.event_log` to RTMR3 and compare it with the
   quote's RTMR3. Check that `SHA-256(evidence.app_compose)` equals the
   measured `compose-hash` event, and that this hash is on your list of
   reviewed releases. The replay follows dstack's `cc-eventlog`; our port is in
   [`clients/verifier-ts/src/dstack-event-log.ts`](../clients/verifier-ts/src/dstack-event-log.ts).
6. **Key.** Take the `x25519-aes-256-gcm-hkdf-sha256-v2.1` entry from this
   keyset. It is the only key the client encrypts to.

Missing evidence fails the check, and the client sends nothing until it
passes. Verify again once `not_after` passes, or when a response carries a
different `X-ACI-Keyset-Digest` (ACI §5.2). Your policy may also require the
key-custody check (ACI §9.1 check 5).

To detect a stream that lost events, also check receipts
([ACI §9.3](../spec/aci.md#93-verify-the-response)): fetch
`/v1/aci/receipts/<X-Receipt-Id>`, verify its Ed25519 signature under the
keyset's receipt key, and compare `response.returned.body_hash` with the
SHA-256 of the raw bytes you received.

## Your API in front of the gateway

Your API becomes a pass-through for the encrypted parts. Check each item:

**Request headers**

- Forward `X-E2EE-Version`, `X-Client-Pub-Key`, `X-Model-Pub-Key`,
  `X-E2EE-Nonce`, `X-E2EE-Timestamp`, and `X-E2EE-Request-Key` unchanged, under
  these names. The client sets them; there is nothing to translate.
- For browser clients, list those six in `Access-Control-Allow-Headers`, and
  `X-E2EE-Applied`, `X-E2EE-Version`, `X-E2EE-Algo`, and `X-Receipt-Id` in
  `Access-Control-Expose-Headers`.

**Request body**

Re-serializing is fine: whitespace and member order do not matter, because the
gateway hashes the canonical JSON form. Changing any value is not. Each of
these fails the request with `e2ee_body_mismatch`:

- mapping a public model name to an internal one;
- adding defaults or members such as `stream_options`, `user`, or routing
  hints;
- removing members the gateway would ignore;
- normalizing numbers or strings.

Make those choices in the client before it encrypts, or carry them in headers.
To debug a mismatch, log `SHA-256(JCS(body))` at the client and at your API's
egress and compare.

**Responses**

- Pass the response body through without changing any JSON value, including
  the `id`, `e2ee_mac`, and SSE event contents. Re-chunking bytes is fine.
  Adding, editing, or removing an event is not.
- Keep the `X-E2EE-*` and `X-Receipt-Id` response headers.

**Retries**

Never resend a request to the gateway yourself. The gateway accepts each
nonce once, so a resend fails with `e2ee_replay_detected`. Return the error and
let the client retry, which builds a new request.

Authentication, rate limits, and billing keep working: they use headers and
the readable parts of the body and response, such as `model`, `max_tokens`,
and `usage`.

## Roll out

1. Enable the gateway's v2.1 support. With `enable_e2ee` on, the reference
   gateway already advertises `"2.1"` and lists the v2.1 key.
2. Make your API forward v2.1 requests and responses as above. Legacy and v2
   requests keep working, so old and new clients can run side by side.
3. Ship clients that use v2.1.
4. When no client sends `X-Signing-Algo` any more, stop accepting it at your
   API.

## Errors you may see

| Type | Usual cause |
| --- | --- |
| `e2ee_body_mismatch` | A body value changed after the client encrypted it, or the client hashed a different body than it sent. |
| `e2ee_decryption_failed` | `X-Client-Pub-Key` or `X-E2EE-Request-Key` changed in transit, or a field was not encrypted. |
| `e2ee_header_missing` | A header was dropped in transit. |
| `e2ee_invalid_header` | An E2EE header was repeated or rewritten in transit. |
| `e2ee_invalid_timestamp` | The client clock is more than 5 minutes off. |
| `e2ee_invalid_version` | The gateway does not offer v2.1, or a v2 request carried `X-E2EE-Request-Key`. |
| `e2ee_replay_detected` | The same request reached the gateway twice. |

On the client side, `E2eeError` from the SDK means a successful response did
not verify: something between the client and the gateway changed it.

## Limits

- Roles, message count, model, sampling parameters, tool definitions, usage
  counts, and ciphertext lengths stay readable. V2.1 stops changes to them, not
  reading them.
- A stream can still lose, repeat, or reorder whole events, or end early, for
  example dropping the event that carries " not". The signed receipt's
  `response.returned.body_hash` covers the exact stream, but the SDK does not
  check receipts, so on its own it does not detect this.
- Replay protection lives in the gateway process. A gateway restart inside the
  5-minute window lets an earlier request run again, though its reply stays
  unreadable to the intermediary.
