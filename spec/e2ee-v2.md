# E2EE v2 Compatibility Protocol

> **Version:** `2`
> **Status:** supported, frozen compatibility protocol
> **Audience:** client and service implementers maintaining E2EE v2 interoperability
> **Conformance language:** MUST, SHOULD, and MAY are used in the RFC 2119 sense

E2EE v2 encrypts content-bearing request and response fields between a client
and an attested inference workload, on top of TLS. It is a client-facing
transport extension to [Attested Confidential Inference (ACI)](aci.md), not
part of the core `aci/1` protocol.

> **Warning:** E2EE v2 will be replaced by E2EE v3. The reference gateway
> supports v2 through at least February 10, 2027. V2 clients should plan to
> migrate once v3 is specified.

## 1. Support and replacement policy

| Item | Policy |
| --- | --- |
| Current status | Supported for existing and in-development v2 clients. |
| Compatibility window | The reference gateway will support v2 through at least **February 10, 2027**, six months from this policy's publication on August 10, 2026. |
| Evolution | V2 is frozen except for security, correctness, and interoperability fixes. New features will not expand its wire contract. |
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
[§4.1](aci.md#41-response)). A service conforms to this extension when it
advertises `"2"` and follows this document. E2EE v2 is not required for core
`aci/1` conformance.

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
legacy compatibility path, not E2EE v2.

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
  reject a repeated `(client_public_key, service_public_key, nonce)` tuple
  within the acceptance window with `e2ee_replay_detected`. For replay
  comparison, the two accepted hex cases encode the same nonce bytes.

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
| `e2ee_header_missing` | 400 | Some but not all five required v2 headers are present. |
| `e2ee_invalid_version` | 400 | Unsupported `X-E2EE-Version`, or the service does not terminate v2. |
| `e2ee_invalid_public_key` | 400 | A supplied public key is not valid for the selected suite. |
| `e2ee_model_key_mismatch` | 400 | `X-Model-Pub-Key` is not an attested service E2EE key. |
| `e2ee_invalid_nonce` | 400 | `X-E2EE-Nonce` is not exactly 64 hexadecimal characters. |
| `e2ee_replay_detected` | 400 | The client-key, service-key, and nonce tuple was already used inside the replay window. |
| `e2ee_invalid_timestamp` | 400 | `X-E2EE-Timestamp` is malformed or outside the acceptance window. |
| `e2ee_invalid_payload_model` | 400 | The request's top-level `model` is absent or not a string. |
| `e2ee_decryption_failed` | 400 | A ciphertext is malformed, AEAD authentication fails, or decrypted content cannot be restored. |
| `e2ee_unsupported_endpoint` | 400 | V2 headers were sent to an endpoint this protocol does not define. |

## 10. Protocol constants

| Set | Values |
| --- | --- |
| Version | `2` |
| Request AAD purpose | `aci.e2ee.request.v2` |
| Response AAD purpose | `aci.e2ee.response.v2` |
| HKDF info | `aci.e2ee.v2.x25519`, `aci.e2ee.v2.secp256k1` |
| Algorithms | `x25519-aes-256-gcm-hkdf-sha256`, `secp256k1-aes-256-gcm-hkdf-sha256` |

Unknown versions and algorithms MUST be rejected. Implementations MUST NOT
guess, negotiate a downgrade, or reinterpret one suite as another.

## 11. References

Normative for this compatibility protocol:

- RFC 7748: X25519 key agreement.
- SEC 1: secp256k1 point encoding and elliptic-curve key agreement.
- RFC 5869: HKDF.
- NIST SP 800-38D: AES-GCM.
- RFC 8785: JSON Canonicalization Scheme (JCS).
