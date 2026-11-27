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

## Client: use the TypeScript SDK

`@phala/aci-verifier` verifies the gateway and encrypts at the transport
layer, so an OpenAI-compatible SDK works unchanged:

```bash
npm install @phala/aci-verifier openai
```

```ts
import OpenAI from 'openai';
import { createE2eeFetch, verifyService } from '@phala/aci-verifier';

// 1. Verify the gateway against the releases you reviewed.
const transcript = await verifyService('https://gateway.example', {
  acceptedComposeHashes: ['<reviewed sha256(app_compose)>'],
});
const { keyset } = transcript.verification;
if (!transcript.verdict.verified || keyset === undefined) {
  throw new Error(transcript.verdict.line);
}

// 2. Encrypt every request and decrypt every reply.
const client = new OpenAI({
  baseURL: 'https://api.your-service.example/v1', // your API in front of the gateway
  apiKey: process.env.API_KEY,
  fetch: createE2eeFetch({ keyset }),
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

- uses v2.1 whenever the verified keyset lists a v2.1 key, and throws if it
  lists none;
- never retries in another mode. An error reply is handed back unchanged; a
  successful reply that fails its MAC or decryption is refused;
- refuses POST requests to endpoints v2.1 does not cover, such as
  `/v1/responses`, and requests with `prediction`, `functions`, or
  `function_call`, which v2.1 does not encrypt;
- builds a new request, with a new nonce, every time it is called, so SDK
  retries are safe.

Without the SDK, implement [§10 of the spec](../spec/e2ee-v2.md#10-version-21)
and check your code against the
[v2.1 test vectors](../spec/e2ee-v2-test-vectors.md#version-21).

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
