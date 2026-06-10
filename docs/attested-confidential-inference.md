# Attested Confidential Inference

This page is the product-neutral source for `{PRODUCT_NAME}` inference docs.
Product docs should replace every placeholder before publishing.

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
| `{PRODUCTION_VERIFIER_POLICY_URL}` | Published verifier policy for accepted gateway workload IDs, source provenance, image digests, KMS roots, and TLS bindings. |

## What Verification Proves

The API returns normal OpenAI-compatible responses and adds verifiable evidence.
A verifier checks two layers:

1. The gateway attestation report proves the workload identity, attested keyset,
   source provenance, freshness, and hardware-backed quote evidence for the
   gateway that served the API.
2. The per-response receipt proves request and response hashes, selected
   upstream verification, transparency events, and the receipt signature under a
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

- Response body bytes.
- `x-receipt-id` response header.
- Optional `id` field from the JSON response.
- `x-aci-identity` and `x-aci-keyset-digest` headers, if present.

`x-receipt-id` is the stable lookup key for verification. The JSON response
`id` can also work when the response body contains a chat completion ID.

## Verification Flow

Generate a fresh nonce before fetching the attestation report.

```bash
NONCE="$(openssl rand -hex 16)"

curl "$API_BASE_URL/v1/attestation/report?nonce=$NONCE" \
  -H "Authorization: Bearer $API_KEY" \
  -o attestation-report.json
```

Fetch the receipt for the response.

```bash
curl "$API_BASE_URL/v1/receipt/$RECEIPT_ID" \
  -H "Authorization: Bearer $API_KEY" \
  -o receipt.json
```

Then verify these facts locally:

1. The attestation report uses `api_version: "aci/1"`.
2. `workload_id` matches the hash of the attested workload identity.
3. `workload_keyset_digest` matches the hash of the attested keyset.
4. The report `report_data` matches the fresh nonce and attested keyset.
5. The keyset endorsement verifies under the workload identity key.
6. The report is fresh at verification time.
7. Hardware evidence verifies against the production policy. This includes quote
   verification, quote report-data binding, event-log replay when present,
   key-custody evidence when present, and source or image provenance.
8. The receipt `workload_id` and `workload_keyset_digest` match the verified
   attestation report.
9. The receipt signature verifies under the receipt key listed in the attested
   keyset.
10. `response.returned.wire_hash` matches the exact response bytes received by
    the client.
11. `request.received.body_hash` matches the gateway-observed request body when
    that body is available to the verifier.
12. `upstream.verified.result` is `verified` for the selected upstream model,
    and its channel binding is enforceable by the gateway.
13. If `upstream.verified.session_id` is present, fetch
    `/v1/audit/sessions/{session_id}` and verify the session record matches the
    receipt event.

The verifier should fail closed if a required artifact is missing, malformed,
expired, unsigned, or rejected by policy.

## Current Artifact Endpoints

| Endpoint | Purpose |
| --- | --- |
| `GET /v1/attestation/report?nonce=<nonce>` | Fresh gateway attestation report. |
| `GET /v1/receipt/{id}` | Signed ACI receipt. `{id}` can be a receipt ID or response chat ID. |
| `GET /v1/signature/{id}` | Legacy compatibility response. New verifiers should read the embedded `receipt`. |
| `GET /v1/receipt/{id}/body` | Optional retained post-rewrite request body. Available only when retention is enabled. |
| `GET /v1/audit/sessions/{session_id}` | Optional attested-session audit record referenced by receipt events. |

## Verification Bundle API

Pre-launch deployments may add a batch endpoint that returns all verification
artifacts in one round trip. This endpoint is an artifact transport, not a trust
oracle. The verifier still checks every signature, hash, attestation binding,
session record, and production policy locally.

```text
POST /v1/verification/bundles
```

Request:

```json
{
  "nonce": "<fresh verifier nonce>",
  "items": [{"id": "<chat id or receipt id>"}],
  "include": {
    "legacy_signature": false,
    "sessions": true,
    "retained_body": false
  }
}
```

Response:

```json
{
  "api_version": "aci.verification_bundle.v1",
  "attestation_report": {},
  "items": [
    {
      "id": "<requested id>",
      "receipt": {},
      "legacy_signature": null,
      "sessions": [],
      "retained_body": null
    }
  ]
}
```

`retained_body` must stay opt-in because it can expose the post-rewrite request
body and only exists when body retention is enabled.

## E2EE Mode

E2EE encrypts selected request and response fields between the client and the
attested gateway. TLS still protects the HTTP connection. E2EE adds field-level
encryption so the gateway can prove the decryption key came from the attested
keyset.

Use ACI E2EE v2 for new clients.

Required headers:

| Header | Value |
| --- | --- |
| `X-E2EE-Version` | `2` |
| `X-Client-Pub-Key` | Client secp256k1 public key, hex encoded. |
| `X-Model-Pub-Key` | Gateway E2EE public key from the attested keyset. |
| `X-E2EE-Nonce` | Unique request nonce. |
| `X-E2EE-Timestamp` | Unix seconds. Must be close to gateway time. |

Do not send `X-Signing-Algo` for ACI E2EE v2. That header selects the legacy
compatibility path.

The ciphertext format is:

```text
ephemeral_uncompressed_secp256k1_public_key || aes_gcm_nonce || ciphertext_tag
```

The concatenated bytes are lowercase hex encoded. The AEAD associated data binds
the ciphertext to the protocol version, direction, algorithm, model, field path,
nonce, timestamp, and response identity. Use the official client or verifier
helpers when available because any AAD mismatch fails decryption.

E2EE already provides integrity for encrypted fields. Receipts are still attached
like normal TLS requests. For E2EE requests, `request.received.body_hash` is the
hash of the gateway-observed decrypted JSON body, not the original encrypted
HTTP body sent by the client. Verifiers should compare request hashes only
against the decrypted body bytes or the retained post-rewrite body when that
artifact is available.

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
The receipt records the upstream verifier result and binding material in
`upstream.verified`. Some upstreams use TLS channel binding. Others use
provider-level E2EE keys. The verifier should rely on the recorded binding only
when the production policy accepts that provider and model path.

## Product Wrapper Checklist

Before embedding this page in a product docs site:

1. Replace every placeholder in the table above.
2. Render the product-specific API key environment variable.
3. Set a real `{DEFAULT_MODEL_ID}` that exists in that product's model catalog.
4. Link `{PRODUCTION_VERIFIER_POLICY_URL}` to the published verifier policy.
5. Hide the verification bundle section until the endpoint is implemented.
6. Keep legacy compatibility sections only where old clients need them.
