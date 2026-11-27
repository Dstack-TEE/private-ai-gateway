# E2EE v2 compatibility protocol

> **Versions:** `2` and `2.1`
> **Status:** supported, frozen compatibility protocol
> **Audience:** client and service implementers maintaining E2EE v2 interoperability
> **Conformance language:** MUST, SHOULD, and MAY are used in the RFC 2119 sense

E2EE v2 encrypts content-bearing request and response fields between a client
and an attested inference workload, on top of TLS. It is a client-facing
transport extension to [Attested Confidential Inference (ACI)](aci.md), not
part of the core `aci/1` protocol.

Version 2.1 (§10) adds a per-request secret that the client seals to the
service key, together with a digest of the request body. Every field key in
both directions depends on the secret, and every response object carries a MAC
keyed from it. An intermediary that can rewrite headers and bodies in front of
the workload can therefore neither read replies, change what the model
receives, nor change a response object. Version 2 does not protect against such
an intermediary: it can replace `X-Client-Pub-Key` with its own key and add
messages encrypted to the service key. Clients use v2.1 whenever the attested
keyset lists a v2.1 key.

> **Warning:** E2EE v2 will be replaced by E2EE v3. The reference gateway
> supports v2 through at least February 10, 2027. V2 clients should plan to
> migrate once v3 is specified.

## 1. Support and replacement policy

| Item | Policy |
| --- | --- |
| Current status | v2 and v2.1 are supported. New and updated clients use v2.1. |
| Compatibility window | The reference gateway will support v2 and v2.1 through at least **February 10, 2027**, six months from this policy's publication on August 10, 2026. |
| Evolution | V2 and v2.1 are frozen except for security, correctness, and interoperability fixes. New features will not expand their wire contracts. |
| Planned successor | E2EE v3 is intended to replace v2. Its protocol and migration path will be specified separately. |

V2 clients can rely on the contract in this document during the compatibility
window. A future retirement notice will identify the v3 replacement and the
required migration steps.

V2 predates ACI's identity-free keyset design. It does not require, and MUST
NOT be interpreted as requiring, a separate workload identity key or keyset
endorsement. The ACI quote binds the workload keyset digest directly, and the
client selects an E2EE key from that quote-bound keyset.

## 2. Relationship to ACI

An ACI service advertises supported client-facing encryption extensions in
`service_capabilities.supported_e2ee_versions` and publishes their keys in
`workload_keyset.e2ee_public_keys` ([ACI §3.1](aci.md#31-workload-keyset) and
[§4.1](aci.md#41-response)). A service conforms to v2 when it advertises `"2"`
and follows this document, and to v2.1 when it also advertises `"2.1"`, lists
v2.1 keys, and follows §10. E2EE is not required for core `aci/1` conformance.

A service advertising v2 MUST support `POST /v1/chat/completions` for both
buffered and streaming responses. It SHOULD support v2 on the other prompt
endpoints it serves when this document defines their field shapes.

Client E2EE terminates at an aggregator. Encryption between an aggregator and
an upstream is a separate channel-binding detail and MUST NOT be advertised as
client-facing v2 support.

## 3. HTTP contract

### 3.1 Request headers

A v2 request sends all five headers:

| Header | Value |
| --- | --- |
| `X-E2EE-Version` | Exactly `2`. |
| `X-Client-Pub-Key` | Client public key, hex encoded using the selected suite's curve. Response fields are encrypted to this key. |
| `X-Model-Pub-Key` | Service E2EE public key selected from the quote-bound ACI workload keyset. |
| `X-E2EE-Nonce` | A fresh 32-byte request replay token encoded as 64 hexadecimal characters (§7). |
| `X-E2EE-Timestamp` | Current Unix time in seconds (§7). |

A v2 request MUST NOT send `X-Signing-Algo`. That header selects the pre-ACI
legacy compatibility path, not E2EE v2. A v2.1 request sends one more header
(§10.2).

### 3.2 Response headers

| Header | Value |
| --- | --- |
| `X-E2EE-Applied` | `true` when response fields were encrypted under this protocol. |
| `X-E2EE-Version` | `2` when E2EE was applied. |
| `X-E2EE-Algo` | The selected service key's `algo`. |

These headers are unauthenticated hints. The quote-bound ACI workload keyset,
each field's AEAD tag, and the signed ACI receipt provide the cryptographic
bindings.

## 4. Algorithms

E2EE v2 defines two cipher suites. Both use ECDH between a fresh ephemeral key
and the recipient's static key. Requests use the service key from the attested
keyset. Responses use the client's `X-Client-Pub-Key`. Both suites use
HKDF-SHA256 and AES-256-GCM.

| `algo` | Curve | Ephemeral key encoding | HKDF `info` |
| --- | --- | --- | --- |
| `x25519-aes-256-gcm-hkdf-sha256` | X25519 | 32 bytes raw | `aci.e2ee.v2.x25519` |
| `secp256k1-aes-256-gcm-hkdf-sha256` | secp256k1 | 65 bytes, uncompressed SEC1 | `aci.e2ee.v2.secp256k1` |

The X25519 suite is RECOMMENDED for browser-native clients. The secp256k1
suite remains supported for existing EVM and dstack clients. A service MUST
publish at least one v2 suite in `e2ee_public_keys` and SHOULD publish X25519.
The client selects a suite by the `algo` of the keyset entry it encrypts to.

The AES-256-GCM key is derived as:

```text
key = HKDF-SHA256(salt = none, ikm = ecdh_shared_secret,
                  info = <suite info string>, len = 32)
```

`ecdh_shared_secret` is the raw X25519 output or the x-coordinate of the
secp256k1 shared point. Each encrypted field value is the lowercase-hex
encoding of:

```text
ephemeral_public_key || aes_gcm_nonce (12 bytes) || ciphertext || tag (16 bytes)
```

A fresh ephemeral key and AES-GCM nonce MUST be used per encrypted field.
Public keys are hex with an optional `0x` prefix. For secp256k1, the 64-byte
uncompressed form without the `0x04` prefix MUST be accepted and treated as
the same key.

## 5. Encrypted fields

The client encrypts field values in place. The surrounding JSON stays
OpenAI-compatible. Each encrypted location has a **field path**: member names
and array indexes from the body root joined with `.`, such as
`messages.3.content`, `messages.1.content.0.image_url.url`,
`choices.0.message.content`, or `data.4.embedding`. For `choices` and `data`,
the index is the entry's `index` member when present and its array position
otherwise. The field path is part of the AAD (§6), so ciphertext cannot be
moved to another location.

Request locations:

| Content | Field path |
| --- | --- |
| whole message content, any modality | `messages.{m}.content` (a string, or a structured content array serialized to JSON) |
| text part | `messages.{m}.content.{c}.text` |
| image part | `messages.{m}.content.{c}.image_url.url` |
| audio part | `messages.{m}.content.{c}.input_audio.data` |
| completion prompt | `prompt`, or `prompt.{i}` per string element |
| embedding input | `input`, or `input.{i}` per string element |

Rules:

- The client SHOULD encrypt every content-bearing field it sends. For a part
  type not listed above, encrypt the whole `messages.{m}.content` value after
  compactly serializing the structured content array.
- A decrypted whole-content plaintext that parses as a JSON array is restored
  as structured content. Any other plaintext is used as a string.
- A request MUST contain at least one encrypted field or the service rejects it
  with `e2ee_decryption_failed`.
- Non-string array elements, such as token IDs in `input`, pass through
  unencrypted.

The service MUST encrypt every generated-content field present in a response:

| Endpoint | Buffered | Streaming (per SSE chunk) |
| --- | --- | --- |
| chat-style | `choices.{i}.message.content`, `choices.{i}.message.reasoning`, `choices.{i}.message.reasoning_content`, `choices.{i}.message.audio.data` | `choices.{i}.delta.content`, `choices.{i}.delta.reasoning`, `choices.{i}.delta.reasoning_content` (an empty-string delta content MAY be omitted) |
| `/v1/completions` | `choices.{i}.text` | `choices.{i}.text` |
| `/v1/embeddings` | `data.{i}.embedding` (compact JSON of the value) | not supported |

An endpoint whose response fields are not defined here MUST reject v2 with
`e2ee_unsupported_endpoint`. It MUST NOT return plaintext content marked as
E2EE-applied.

## 6. Associated data

Every ciphertext is bound to its field and request context through AES-GCM
associated data. The AAD is RFC 8785 JCS canonical JSON:

```text
request field:
  aad = JCS({
    "purpose": "aci.e2ee.request.v2",
    "algo":    <service E2EE key algo>,
    "model":   <request model>,
    "field":   <field path>,
    "nonce":   <X-E2EE-Nonce>,
    "ts":      <X-E2EE-Timestamp, integer>
  })

response field:
  aad = JCS({
    "purpose": "aci.e2ee.response.v2",
    "algo":    <service E2EE key algo>,
    "model":   <request model>,
    "id":      <response id>,
    "field":   <field path>,
    "nonce":   <X-E2EE-Nonce>,
    "ts":      <X-E2EE-Timestamp, integer>
  })
```

Components:

- `algo` is the selected service E2EE key's algorithm.
- `model` is the request's top-level `model` string, byte-exact, with no
  trimming, case folding, alias expansion, or Unicode normalization. Response
  AAD uses the request model, not an upstream response model. A missing or
  non-string model is rejected with `e2ee_invalid_payload_model`.
- `field` is the encrypted location's field path (§5).
- `id` is the clear response `id`, or `""` when the response has none.
- `nonce` and `ts` come from `X-E2EE-Nonce` and `X-E2EE-Timestamp`.

Byte-exact AAD examples are in the
[E2EE v2 test vectors](e2ee-v2-test-vectors.md).

## 7. Key selection, validation, and replay protection

`X-Model-Pub-Key` MUST equal a service `e2ee_public_keys` entry carrying a
§4 suite. Otherwise the request is rejected with
`e2ee_model_key_mismatch`. This proves the client encrypted to a key it could
have established through ACI attestation.

- `X-E2EE-Timestamp` is Unix seconds. The service MUST reject a request when
  `|now - timestamp| > 300`, or outside a narrower published window
  (`e2ee_invalid_timestamp`).
- `X-E2EE-Nonce` is 32 random bytes encoded as exactly 64 hexadecimal
  characters, either case, with no `0x` prefix. The service rejects malformed
  values with `e2ee_invalid_nonce`.
- The client MUST generate a fresh E2EE nonce per request. The service MUST
  record the `(service_public_key, nonce)` tuple only after successful request
  decryption and reject a repeated tuple with `e2ee_replay_detected` throughout
  the timestamp acceptance window, including `now = timestamp + 300` for the
  default window. For replay comparison, the service public key is normalized
  according to its suite, and the two accepted hex cases encode the same nonce
  bytes.

A malformed client public key is rejected with `e2ee_invalid_public_key`.
An `X-E2EE-Version` other than `2` is rejected with
`e2ee_invalid_version`. A request missing a required v2 header is rejected
with `e2ee_header_missing`. Decryption, ciphertext-format, or JSON restoration
failures are rejected with `e2ee_decryption_failed`.

Each field's AES-GCM tag authenticates that field and its AAD. Stream order and
truncation are checked against the signed ACI receipt over the complete wire
bytes ([ACI §7.4](aci.md#74-event-vocabulary) and
[§9.3](aci.md#93-verify-the-response)).

Replicas that share a v2 keyset MUST also share replay state or ensure a
request cannot reach more than one replica inside the acceptance window.

## 8. ACI receipt integration

V2 uses the receipt format defined by ACI. It does not add receipt fields.

- The receipt `model` is the clear top-level request model bound into the AAD.
- `request.received.body_hash` covers the compact JSON serialization after
  encrypted request fields are decrypted and restored, not the encrypted wire
  body.
- `response.returned.body_hash` covers the exact JSON or SSE bytes emitted on
  the wire, including encrypted response fields and streaming framing.
- The client reproduces `request.received.body_hash` by replacing ciphertext
  fields with their decrypted values and compactly serializing the restored
  body.
- The client verifies `response.returned.body_hash` against the exact response
  wire bytes and separately verifies each decrypted field's AEAD tag and AAD.

## 9. Error types

V2 errors use the OpenAI-compatible ACI error shape. The service SHOULD use
these HTTP statuses and MUST preserve the `type` when an intermediary requires
a different status.

| Type | Status | Meaning |
| --- | --- | --- |
| `e2ee_header_missing` | 400 | Some but not all required headers are present: five for v2, six for v2.1. |
| `e2ee_invalid_header` | 400 | v2.1 only: an E2EE request header appears more than once, or its value is not visible ASCII. v2 and legacy requests read such a value as before: a non-visible-ASCII value as absent, a repeated header as its first value. |
| `e2ee_invalid_version` | 400 | Unsupported `X-E2EE-Version`, the service does not terminate that version, or a v2 request carries `X-E2EE-Request-Key`. |
| `e2ee_invalid_public_key` | 400 | A supplied public key is not valid for the selected suite. |
| `e2ee_model_key_mismatch` | 400 | `X-Model-Pub-Key` is not an attested service E2EE key. |
| `e2ee_invalid_nonce` | 400 | `X-E2EE-Nonce` is not exactly 64 hexadecimal characters. |
| `e2ee_replay_detected` | 400 | The normalized service-key and nonce tuple was already used inside the timestamp acceptance window. |
| `e2ee_invalid_timestamp` | 400 | `X-E2EE-Timestamp` is malformed or outside the acceptance window. |
| `e2ee_invalid_payload_model` | 400 | The request's top-level `model` is absent or not a string. |
| `e2ee_decryption_failed` | 400 | A ciphertext is malformed, AEAD authentication fails, or decrypted content cannot be restored. |
| `e2ee_body_mismatch` | 400 | v2.1: the request body does not match the digest sealed in `X-E2EE-Request-Key`. |
| `e2ee_unsupported_endpoint` | 400 | V2 headers were sent to an endpoint this protocol does not define. |

## 10. Version 2.1

Version 2.1 keeps §3–§9 for the X25519 suite and adds a **request secret**:
32 random bytes the client creates for each request. The client seals the
secret, together with a digest of the request body, to the service key. Every
field key in both directions mixes the secret in, and every response object
carries a MAC keyed from it. Without the secret, an intermediary cannot:

- read replies, even after replacing `X-Client-Pub-Key` with its own key;
- add, remove, reorder, or edit request messages, or change readable request
  parameters such as `max_tokens`, `tools`, or a message `role`;
- change, add, or remove anything inside a response object, including readable
  members such as a tool call's `name`, `finish_reason`, or `usage`;
- drop, repeat, or reorder SSE events, except by ending the stream early;
- downgrade the request to v2, because fields sealed under v2.1 do not open
  under the v2 key derivation.

It can still end a stream early. The receipt detects this (§8, §10.8).

### 10.1 Keys and selection

A service that terminates v2.1 advertises `"2.1"` in `supported_e2ee_versions`
and lists its X25519 key in `e2ee_public_keys` under the `algo`
`x25519-aes-256-gcm-hkdf-sha256-v2.1`. Curve, encodings, and AEAD are those of
`x25519-aes-256-gcm-hkdf-sha256` (§4). V2.1 has no secp256k1 suite. The v2.1
entry MAY carry the same key material and `key_id` as a v2 entry; the reference
gateway lists its X25519 key a second time this way.

The v2.1 entry is what tells a client that v2.1 is available. The quote does
not cover `service_capabilities` ([ACI §4.1](aci.md#41-response)), but it does
cover the keyset, so an intermediary cannot remove a v2.1 entry from a report
the client verified with a fresh nonce. A client that finds one:

- MUST use v2.1 for every E2EE request to that service;
- MUST NOT fall back to v2, the legacy path, or plaintext after an error, a
  timeout, or a missing capability. Error responses are not authenticated, so
  anyone on the path can forge them;
- MUST reject a successful response that fails §10.6, including one marked
  `X-E2EE-Applied: false`.

### 10.2 Request headers

A v2.1 request sends the five §3.1 headers, with `X-E2EE-Version: 2.1` and
`X-Model-Pub-Key` set to the v2.1 entry's key, plus:

| Header | Value |
| --- | --- |
| `X-E2EE-Request-Key` | The sealed request secret and body digest (§10.3), in the §4 field encoding. |

A v2 request MUST NOT send `X-E2EE-Request-Key`.

### 10.3 Request key

The client:

1. Draws a fresh 32-byte request secret from a CSPRNG.
2. Encrypts every request field (§10.5) with the v2.1 field key (§10.4).
3. Computes `body_digest = SHA-256(JCS(body))` over the final JSON request
   body, with the ciphertext fields in place. JCS is RFC 8785, including its
   number formatting: every number is written as the ECMAScript form of its
   IEEE 754 double.
4. Seals the 64 bytes `request_secret || body_digest` to the service key with
   the §4 construction, the request-key HKDF info (§10.4), and this AAD:

```text
aad = JCS({
  "purpose":        "aci.e2ee.request-key.v2.1",
  "algo":           "x25519-aes-256-gcm-hkdf-sha256-v2.1",
  "model":          <request model>,
  "client_pub_key": <X-Client-Pub-Key as 32-byte lowercase hex>,
  "nonce":          <X-E2EE-Nonce>,
  "ts":             <X-E2EE-Timestamp, integer>
})
```

A replaced `X-Client-Pub-Key` therefore fails to open the request key.

The digest covers the parsed JSON, not its bytes. An intermediary may
re-serialize the body, changing whitespace or member order, without breaking
it. Adding, removing, or changing a member, an array entry, a string, a
literal, or a number's double value breaks it. JCS normalizes number forms, so
re-encoding a number as another form of the same double is not detected:
`1`, `1.0`, and `1e0` hash alike, and an integer beyond ±2^53, such as a large
`seed`, hashes as its double, like any integer that rounds to the same double.
A JavaScript client sends the digits of that double; other clients may send
the exact integer, and the service forwards the integer as received.

### 10.4 Key derivation

V2.1 fields use the §4 X25519 ECDH and wire format. The HKDF step differs, and
a third key authenticates responses:

```text
field key       = HKDF-SHA256(salt = request_secret, ikm = ecdh_shared_secret,
                              info = "aci.e2ee.v2.1.x25519", len = 32)
request-key key = HKDF-SHA256(salt = none, ikm = ecdh_shared_secret,
                              info = "aci.e2ee.v2.1.x25519.request-key", len = 32)
MAC key         = HKDF-SHA256(salt = none, ikm = request_secret,
                              info = "aci.e2ee.v2.1.response-mac", len = 32)
```

The service still encrypts each response field to `X-Client-Pub-Key` with a
fresh ephemeral key. Reading a reply therefore takes both the client's private
key and the request secret. Clients SHOULD use a fresh client key per request.

### 10.5 Fields

Field AAD is the §6 object with the purposes `aci.e2ee.request.v2.1` and
`aci.e2ee.response.v2.1`, and `algo` set to the v2.1 algo.

V2.1 encrypts the locations below. They include every §5 location except
per-part request content, which v2.1 encrypts whole. In a chat message, `{p}`
is `messages.{m}` in a request (history the client sends back) and, in a
response, both `choices.{i}.message` and `choices.{i}.delta` whenever present,
in a buffered body and in a stream alike. The same message locations apply in
both directions:

| Where | Field paths |
| --- | --- |
| chat message, request and response | `{p}.content`, `{p}.reasoning`, `{p}.reasoning_content`, `{p}.refusal`, `{p}.annotations`, `{p}.reasoning_details`, `{p}.thinking_blocks`, `{p}.reasoning_items`, `{p}.images`, `{p}.audio.data`, `{p}.audio.transcript`, `{p}.function_call.arguments`, `{p}.tool_calls.{t}.function.arguments`, `{p}.tool_calls.{t}.custom.input` |
| chat request | `prediction.content` |
| completions request | `prompt` or `prompt.{n}`, `suffix` |
| embeddings request | `input` or `input.{n}`, and `messages` as in chat |
| chat and completions response | `choices.{i}.logprobs`, `choices.{i}.token_ids`, `choices.{i}.prompt_token_ids`, `choices.{i}.prompt_logprobs`, `prompt_token_ids`, `prompt_logprobs` |
| completions response | `choices.{i}.text` |
| embeddings response | `data.{i}.embedding` |

The `token_ids` and `prompt_*` members are vLLM's echoes of the generated and
prompt tokens. `{i}` and `{t}` are the entry's `index` member when it is a
non-negative integer up to 2^53 − 1, the range a JavaScript `JSON.parse`
keeps exact, and its array position otherwise. A number with a zero fraction,
such as `1.0`, is that integer; the path is written in integer form
(`…tool_calls.1…`). `{m}` and `{n}` are array positions.

**Plaintext.** Every v2.1 plaintext is the compact JSON encoding of the
location's value, whatever its type: the text `hello` is sealed as `"hello"`,
an `arguments` object or a content array as that object or array. The
receiver restores the value by parsing that JSON, so a value of an unexpected
type is neither refused nor left readable, and text that looks like JSON stays
text. A plaintext that is not JSON fails with `e2ee_decryption_failed` in a
request and fails the response at the client.

**Token ids.** In `prompt` and `input`, the whole value or an array item that
is a number or an array of numbers is token ids. It stays readable and unsealed,
as in §5; every other item, such as text or a multimodal object, is sealed.

The client MUST seal every present, non-null value at these locations, and the
service seals every one in a response. A `null` or absent value is skipped.
The service fails a request with `e2ee_decryption_failed` when a location
holds anything but a ciphertext string or `null`, for example a string or
parts in clear. Unlike v2, a v2.1 request need not contain an encrypted field:
the request key already binds the body, so a token-id prompt is valid.

#### Readable members

Everything else in a v2.1 request is sent as the client wrote it, so a v2.1
request can carry every member a plaintext request can: sampling parameters,
`tools` and `functions`, `response_format`, `metadata`, `user`,
`prompt_cache_key`, `cache_salt`, `chat_template_kwargs`, `thinking`,
`modalities` and `audio`, `provider`, vendor extensions, and the readable
parts of history a reply echoes back, such as a tool call's `id` and `name` or
`audio: {"id": …}`. The service forwards them unchanged.

Readable members are visible to every intermediary, but the body digest
(§10.3) binds them: none can be added, removed, or changed without
`e2ee_body_mismatch`, apart from the number re-encodings JCS normalizes. Applications MUST NOT put private text in a readable
member, for example in `metadata`, `user`, a tool description, or a vendor
extension. A future version should encrypt the whole body instead, for
example with HPKE or OHTTP.

### 10.6 Response MAC

The service adds a member `e2ee_mac` to every response object it encrypts: the
buffered JSON body, and the JSON of each SSE `data:` event. Its value is the
lowercase-hex HMAC-SHA256, under the MAC key (§10.4), of the JCS form of the
object without `e2ee_mac`, computed after the fields are encrypted. JCS
writes numbers in ECMAScript form, so a client outside JavaScript must too:
`usage.cost` 4.5e-06 is written `0.0000045`, and a MAC over a plain JSON dump
of the parsed object fails.

In a stream, the service first adds `e2ee_seq` to every event object: the
event's position among the stream's JSON events, the integer 0 for the first
and one more for each next one. The MAC covers it. A buffered body has no
`e2ee_seq`.

The client MUST, for each response object:

1. require the response kind to match the request: an SSE stream for
   `"stream": true`, a JSON body otherwise;
2. require `e2ee_mac` and check it, then remove it;
3. in a stream, require `e2ee_seq` to equal the number of events accepted
   before this one, then remove it;
4. require `choices` and `data`, when present, to be arrays;
5. in a stream, require every event's `id` to equal the first event's;
6. decrypt every §10.5 location that is present.

In a stream, the client passes `data: [DONE]` through, ignores SSE comment
lines, and rejects any other line and any event after `[DONE]`. Error
responses (HTTP 4xx and 5xx) carry no MAC and are not authenticated.

### 10.7 Service processing

The service checks a v2.1 request in this order and rejects it at the first
failure, before any upstream request:

1. Required headers and version (§3.1, §10.2).
2. Nonce format and timestamp window (§7).
3. `X-Model-Pub-Key` equals the v2.1 entry: `e2ee_model_key_mismatch`.
4. `X-Client-Pub-Key` is a valid X25519 key: `e2ee_invalid_public_key`.
5. The body is JSON (`e2ee_decryption_failed`) with a string `model`
   (`e2ee_invalid_payload_model`).
6. `X-E2EE-Request-Key` opens under the §10.3 AAD and holds 64 bytes:
   `e2ee_decryption_failed`.
7. The sealed digest equals the §10.3 digest of the received body:
   `e2ee_body_mismatch`.
8. Every field opens under the v2.1 field key: `e2ee_decryption_failed`.
9. The replay tuple is unused (§7): `e2ee_replay_detected`.

V2 and v2.1 share replay state for the same key material. A successful v2.1
response carries `X-E2EE-Version: 2.1` and the v2.1 algo in `X-E2EE-Algo`.
Receipts follow §8 unchanged.

### 10.8 Limits

- Structure and metadata stay readable: roles, message count, model, sampling
  parameters, tool definitions, usage counts, and ciphertext lengths. The body
  digest and the response MAC stop changes to them, not reading them.
- A stream can still end early, after any event: `e2ee_seq` stops lost,
  repeated, and reordered events, but not truncation. The receipt's
  `response.returned.body_hash` covers the exact stream (§8), so detecting
  truncation takes a receipt check over the raw bytes received.
- Request members outside §10.5 stay readable (§10.5, Readable
  members), and so do token ids in `prompt` and `input`.
- Response members outside §10.5 stay readable; the MAC stops changes to
  them. They include a tool call's `name` and `id`, `audio.id`, a choice's
  `finish_reason` and in-band `error`, and members an upstream adds that the
  service does not know.
- An intermediary can still drop or delay requests.
- Replay state lives in the service process. A service restart inside the
  timestamp window lets an earlier request run again. The intermediary still
  cannot read the new reply.
- The service key is long-lived. Anyone who later obtains it can open recorded
  request keys and requests. Replies also need the client's private key.

## 11. Protocol constants

| Set | v2 | v2.1 |
| --- | --- | --- |
| Version | `2` | `2.1` |
| Request AAD purpose | `aci.e2ee.request.v2` | `aci.e2ee.request.v2.1` |
| Response AAD purpose | `aci.e2ee.response.v2` | `aci.e2ee.response.v2.1` |
| Request-key AAD purpose | — | `aci.e2ee.request-key.v2.1` |
| HKDF info | `aci.e2ee.v2.x25519`, `aci.e2ee.v2.secp256k1` | `aci.e2ee.v2.1.x25519`, `aci.e2ee.v2.1.x25519.request-key`, `aci.e2ee.v2.1.response-mac` |
| Response MAC member | — | `e2ee_mac` |
| Stream position member | — | `e2ee_seq` |
| Algorithms | `x25519-aes-256-gcm-hkdf-sha256`, `secp256k1-aes-256-gcm-hkdf-sha256` | `x25519-aes-256-gcm-hkdf-sha256-v2.1` |

Unknown versions and algorithms MUST be rejected. Implementations MUST NOT
guess, negotiate a downgrade, or reinterpret one suite as another.

## 12. References

Normative for this compatibility protocol:

- RFC 7748: X25519 key agreement.
- SEC 1: secp256k1 point encoding and elliptic-curve key agreement.
- RFC 5869: HKDF.
- NIST SP 800-38D: AES-GCM.
- RFC 8785: JSON Canonicalization Scheme (JCS).
- RFC 2104: HMAC.
