# Attested Confidential Inference

This page is the product-neutral source for `{PRODUCT_NAME}` inference docs.
Product docs should replace every placeholder before publishing. The
normative protocol definition is the [ACI Spec](../spec/aci.md).

Primary reader: developers who call the OpenAI-compatible API and verifiers who
need to prove which attested gateway served a response.

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
A verifier checks two layers:

1. The gateway attestation report proves which workload keyset serves the API:
   the hardware quote binds the keyset digest and the verifier's fresh nonce,
   and the report carries source provenance and evidence.
2. The per-response receipt proves the request and response hashes, the
   selected upstream verification outcome, and the receipt signature under a
   key from the attested keyset.

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
    ]
  }'
```

Save these response values:

- Response body bytes, exactly as received off the wire.
- `x-receipt-id` response header.
- Optional `id` field from the JSON response.
- `x-aci-keyset-digest` header, if present.

`x-receipt-id` is the stable lookup key for verification. The JSON response
`id` can also work when the response body contains a chat completion ID.

## Verification Flow

Generate a fresh nonce before fetching the attestation report.

```bash
NONCE="$(openssl rand -hex 16)"

curl "$API_BASE_URL/v1/aci/attestation?nonce=$NONCE" \
  -o attestation-report.json
```

Fetch the receipt for the response.

```bash
curl "$API_BASE_URL/v1/aci/receipts/$RECEIPT_ID" \
  -H "Authorization: Bearer $API_KEY" \
  -o receipt.json
```

Then verify locally. First establish the workload identity (spec §10.1):

1. The hardware quote verifies to the TEE vendor root and binds `report_data`.
2. The binding chain recomputes: base64-decode `workload_keyset_b64`, hash the
   bytes to `workload_keyset_digest`, build the §4.2 statement for your nonce,
   and check its hash equals `report_data`.
3. The keyset is not expired (`now < not_after`).
4. The source provenance is acceptable to the production policy.
5. Private-key custody evidence satisfies the policy (for this implementation,
   the dstack KMS chain in the report evidence).
6. The channel you use is bound: the observed TLS SPKI or the E2EE key you
   encrypt to is listed in the attested keyset.

Then verify the inference (spec §10.2):

1. The receipt envelope signature (Ed25519 over the decoded `payload_b64`
   bytes) verifies under the keyset entry `key_id` names.
2. The payload's `workload_keyset_digest` matches the established digest.
3. `request.received.body_hash` matches the request bytes you sent (for E2EE,
   the original bytes you sealed).
4. `response.returned.body_hash` matches the response bytes you received (the
   raw SSE stream for streaming, the sealed envelope bytes for E2EE).

For aggregator deployments, audit the upstream (spec §10.3): the
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
| `GET /v1/aci/sessions/{hex}` | Attested-session record referenced by receipt events. |
| `GET /v1/aci/sessions?upstream_name=&model=` | List a provider's current attested sessions. |
| `GET /v1/attestation/report` · `GET /v1/signature/{id}` | Legacy dstack-vllm-proxy aliases. New verifiers should use the `/v1/aci/*` endpoints above. |

## Tracing a receipt to its session

The artifacts are linked, not bundled. A receipt's `upstream.verified` event
carries the content-addressed `session_id`; the typed claims, channel
bindings, and evidence live on the session record. Follow the reference to
`GET /v1/aci/sessions/{hex}`: an immutable record the verifier re-checks
itself. Because the session id is the SHA-256 of the served record bytes, the
session you fetch is exactly the one the receipt committed to — race-free,
and permanently cacheable.

The gateway never stores request bodies, so there is no body to fetch: a
service-side rewrite (if any) is committed by `request.forwarded.body_hash`
differing from `request.received.body_hash`.

## E2EE Mode

E2EE seals the whole request and response bodies between the client and the
attested gateway, on top of TLS, so the decryption capability itself is
attested even when TLS terminates outside the workload.

Use ACI E2EE v3 (spec §7). Required headers:

| Header | Value |
| --- | --- |
| `X-E2EE-Version` | `3` |
| `X-Client-Pub-Key` | Client X25519 public key, hex encoded. |
| `X-Model-Pub-Key` | Gateway X25519 E2EE public key from the attested keyset. |

Do not send `X-Signing-Algo` for ACI E2EE. That header selects the legacy
compatibility path.

The request body is the envelope `{ "model": "<id>", "sealed_b64": "<base64>" }`,
where the sealed unit is

```text
ephemeral_x25519_public_key (32) || aes_gcm_nonce (12) || ciphertext || tag (16)
```

sealing your entire original request-body bytes to the attested key
(X25519 + HKDF-SHA256 + AES-256-GCM). The AAD binds the direction context and
the envelope `model`, so a sealed body cannot be replayed under a different
model. Responses come back as `{ "sealed_b64": ... }` — the whole body when
buffered, each SSE event's data payload when streaming.

The gateway unseals your exact original bytes and processes those, so the
receipt's `request.received.body_hash` is reproducible from the bytes you
sealed. For E2EE responses, `response.returned.body_hash` covers the sealed
envelope bytes you received; the AEAD already authenticates the plaintext
inside them.

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
ACI E2EE requests are decrypted inside the attested gateway. If middleware is
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
