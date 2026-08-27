# Attested Confidential Inference

This page is the product-neutral source for `{PRODUCT_NAME}` inference docs.
Product docs should replace every placeholder before publishing. The
normative protocol definition is the [ACI Spec](../spec/aci.md).

Primary reader: developers who call the OpenAI-compatible API and verifiers who
need to prove which attested gateway served a response.

> [!IMPORTANT]
> Upstream verification is a request constraint, not a global gateway mode.
> Set `provider.aci_verified` to `true`, or provide a non-empty
> `provider.aci_session_ids` allowlist, when the request must fail closed unless
> the selected upstream verifies.

## Placeholders

| Placeholder | Meaning |
| --- | --- |
| `{PRODUCT_NAME}` | Product name shown in the wrapper docs. |
| `{API_BASE_URL}` | Base URL without the `/v1` suffix, for example `https://api.example.com`. |
| `{API_KEY_ENV_VAR}` | Environment variable that holds the model API key. |
| `{API_KEY_SOURCE}` | Dashboard, console, or account flow where users create the API key. |
| `{DEFAULT_MODEL_ID}` | Model ID used in quickstart examples. |
| `{PRODUCTION_VERIFIER_POLICY_URL}` | Published verifier policy for accepted source provenance, image digests, keyset subjects, KMS roots, and TLS bindings. |

## What Verification Proves

The API returns normal OpenAI-compatible responses and adds verifiable evidence.
A verifier answers three separate questions:

1. The gateway attestation report proves which workload keyset serves the API:
   the hardware quote binds the keyset digest and the verifier's fresh nonce,
   and the report carries source provenance and evidence.
2. The per-response receipt proves request and response hashes, selected
   upstream verification, and the receipt signature under a key from the
   attested keyset.
3. For an aggregator, the receipt's `upstream.verified` event and cited session
   show which verified provider channel was used and preserve the claims,
   binding, and evidence for independent policy appraisal.

Verification does not rely on the product API server saying "verified". The
verifier fetches artifacts, validates signatures and hashes locally, and applies
the production verifier policy from `{PRODUCTION_VERIFIER_POLICY_URL}`.

## Quick Request

Create an API key from `{API_KEY_SOURCE}` and keep it in `{API_KEY_ENV_VAR}`.
The neutral snippets below copy that value into `API_KEY`; product docs can
render the final environment variable name directly.

```bash
export API_BASE_URL="{API_BASE_URL}"
export API_KEY="<value from {API_KEY_ENV_VAR}>"
export MODEL="{DEFAULT_MODEL_ID}"

curl "$API_BASE_URL/v1/chat/completions" \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "'"$MODEL"'",
    "messages": [
      {"role": "user", "content": "Explain why attestation matters in one sentence."}
    ],
    "provider": {"aci_verified": true}
  }'
```

Save these response values:

- Response body bytes.
- `x-receipt-id` response header.
- Optional `id` field from the JSON response.
- `x-aci-keyset-digest` header, if present.

`x-receipt-id` is the stable lookup key for verification. The JSON response
`id` can also work when the response body contains a chat completion ID.
This example is ACI-constrained, so the gateway will not commit the stream
before it can name the receipt. An unconstrained, non-E2EE middleware stream
may instead start with a keepalive before an upstream is selected and omit the
header. After a successful stream its receipt is available by response `id`;
if forwarding fails after that early commit, no receipt is drafted. A verifier
that requires a receipt should constrain the request.

## Verification Flow

Generate a fresh nonce before fetching the attestation report.

```bash
NONCE="$(openssl rand -hex 32)"

curl "$API_BASE_URL/v1/aci/attestation?nonce=$NONCE" \
  -o attestation-report.json
```

Fetch the receipt for the response.

```bash
curl "$API_BASE_URL/v1/aci/receipts/$RECEIPT_ID" \
  -H "Authorization: Bearer $API_KEY" \
  -o receipt.json
```

Then verify locally. First establish the workload identity (spec §9.1):

1. The hardware quote verifies to the TEE vendor root and binds `report_data`.
2. The binding chain recomputes: hash the served `workload_keyset` object's
   JCS form to `workload_keyset_digest`, build the §3.2 statement for your
   nonce, and check its hash equals `report_data`.
3. The keyset is not expired (`now < not_after`).
4. The source provenance is acceptable to the production policy.
5. Private-key custody evidence satisfies the policy (for this implementation,
   the dstack KMS chain in the report evidence).
6. The channel you use is bound: the observed TLS SPKI or the E2EE key you
   encrypt to is listed in the attested keyset.

Then verify the response (spec §9.3):

1. The receipt signature (Ed25519 over the JCS form of the document
   bytes) verifies under the keyset entry `key_id` names.
2. The payload's `workload_keyset_digest` matches the established digest.
3. `request.received.body_hash` matches the request bytes the gateway processed.
   For E2EE v2, reconstruct the compact JSON body with decrypted field values.
4. `response.returned.body_hash` matches the response bytes you received (the
   raw SSE stream for streaming, including encrypted E2EE fields).

For aggregator deployments, verify the cited session (spec §9.2): the
`upstream.verified` event is `verified` and cites a `session_id`; the fetched
session bytes hash to that id; the evidence data hashes to its digest; the
session's claims satisfy your policy.

The verifier should fail closed if a required artifact is missing, malformed,
expired, unsigned, or rejected by policy.

## Current Artifact Endpoints

| Endpoint | Purpose |
| --- | --- |
| `GET /v1/aci/attestation?nonce=<nonce>` | Fresh gateway attestation report. |
| `GET /v1/aci/receipts/{id}` | Signed ACI receipt. `{id}` can be a receipt ID or response chat ID. |
| `GET /v1/aci/sessions/{session_id}` | Attested-session record referenced by receipt events. |
| `GET /v1/aci/sessions?upstream_name=&model=` | List a provider's current attested sessions. |
| `GET /v1/attestation/report` · `GET /v1/signature/{id}` | Legacy dstack-vllm-proxy aliases, with the `X-Signing-Algo` legacy E2EE mode. Kept for pre-ACI clients under the Appendix B rule: they never alter ACI artifacts, and their report bindings use their own quotes. New verifiers should use the `/v1/aci/*` endpoints above. |

## Tracing a receipt to its session

The artifacts are linked, not bundled. A receipt's `upstream.verified` event
carries the content-addressed `session_id`; the typed claims, channel
bindings, and evidence live on the session record. For a deep audit, follow
that reference to `GET /v1/aci/sessions/{session_id}`: an immutable record with
the full evidence and per-claim reasons, which the verifier re-checks itself.
Because `session_id` is a content hash, the session you fetch is exactly the one
the receipt committed to — race-free, and permanently cacheable.

The gateway never stores request bodies, so there is no body to fetch: the
rewrite (if any) is committed by `request.forwarded.body_hash` differing from
`request.received.body_hash`, not by warehousing plaintext.

## Pinning Sessions on a Request

The link also runs forward. A request body MAY carry serving constraints
(spec §5.3):

```json
"provider": { "aci_verified": true, "aci_session_ids": ["<session-id>", "..."] }
```

`aci_verified` requires a verified attested session for this request;
`aci_session_ids` requires one of the listed sessions — verify candidates from
`GET /v1/aci/sessions` first, then pin the ids you accept. When no listed
session can serve, the gateway refuses with `session_not_accepted` (412)
before forwarding, and the refusal carries its own receipt. The whole
`provider` member is consumed by the gateway and never reaches an upstream.

## E2EE v2 Compatibility Extension

E2EE v2 encrypts content-bearing request and response fields between the
client and the attested gateway. It is a separate transport extension, not
part of the core ACI specification. It is enabled by default and remains
supported through at least February 10, 2027. E2EE v3 is the planned
replacement. V2 is frozen except for security, correctness, and
interoperability fixes.

The normative contract is the
[E2EE v2 compatibility protocol](../spec/e2ee-v2.md). It defines these five
request headers:

| Header | Value |
| --- | --- |
| `X-E2EE-Version` | `2` |
| `X-Client-Pub-Key` | Client public key, hex encoded with the selected suite's curve. |
| `X-Model-Pub-Key` | Gateway E2EE public key from the attested keyset. |
| `X-E2EE-Nonce` | A fresh 32-byte random value encoded as 64 hex characters. |
| `X-E2EE-Timestamp` | Current Unix time in seconds. |

Do not send `X-Signing-Algo` for E2EE v2. That header selects the legacy
compatibility path.

The client selects either `x25519-aes-256-gcm-hkdf-sha256` or
`secp256k1-aes-256-gcm-hkdf-sha256` by matching the `algo` on an
`e2ee_public_keys` entry. Each encrypted field is lowercase hex of:

```text
ephemeral_public_key || aes_gcm_nonce (12) || ciphertext || tag (16)
```

The JSON structure stays OpenAI-compatible. Encrypt request content in place,
for example `messages.0.content`, and decrypt the corresponding response fields
such as `choices.0.message.content`. The RFC 8785 JCS AAD binds each field to
the direction, selected algorithm, request model, full field path, request
nonce, timestamp, and response id. See
[E2EE v2 §5](../spec/e2ee-v2.md#5-encrypted-fields) and
[§6](../spec/e2ee-v2.md#6-associated-data) for the complete contract.

The quote binds the workload keyset digest directly. V2 does not require, and
does not reintroduce, a separate workload identity key or keyset endorsement.
The attested `e2ee_public_keys` entry is the key-provenance anchor.

`enable_e2ee` defaults to `true`. Setting it to `false` is an explicit
operator opt-out: the attestation advertises no supported E2EE versions and
the gateway rejects v2 requests before decryption.

## Legacy Compatibility

Existing vLLM-proxy-compatible clients can continue to use:

- `GET /v1/attestation/report?signing_algo=...`
- `GET /v1/signature/{id}`
- Legacy E2EE headers with `X-Signing-Algo`

Those surfaces exist for compatibility. New verification should treat the ACI
receipt as the primary per-response proof and the attested keyset as the source
of receipt-signing and E2EE keys.

## Trust Boundary

Plain TLS requests are visible to the attested gateway after TLS termination.
E2EE v2 requests are decrypted inside the attested gateway. If middleware is
enabled, middleware is part of the same deployment trust boundary and can see
plaintext after gateway decryption.

Upstream model providers are verified before the gateway forwards request bytes.
The receipt records the upstream verification outcome in `upstream.verified`;
the enforced channel binding is recorded on the cited session. Some upstreams
use TLS channel binding. Others use provider-level E2EE keys. The verifier
should rely on the recorded binding only when the production policy accepts
that provider and model path.

## Product Wrapper Checklist

Before embedding this page in a product docs site:

1. Replace every placeholder in the table above.
2. Render the product-specific API key environment variable.
3. Set a real `{DEFAULT_MODEL_ID}` that exists in that product's model catalog.
4. Link `{PRODUCTION_VERIFIER_POLICY_URL}` to the published verifier policy.
5. Keep legacy compatibility sections only where old clients need them.
