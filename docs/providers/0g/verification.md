# 0G — provider-reported per-response verification

- **TEE:** provider-reported by 0G per response.
- **Session binding:** none enforced by this gateway in the first adapter.
- **Verifier:** first-party Rust adapter (`provider: "0g"`).
- **Status:** fail-closed for buffered responses only; streaming is intentionally
  unsupported until response verification can be bound before bytes are released.

## What Is Enforced

For every routed request, the gateway rewrites the OpenAI-compatible JSON body
after model aliasing and forces the top-level field:

```json
{
  "verify_tee": true
}
```

The receipt's `request.forwarded.body_hash` covers these exact routed bytes, so
a caller can see that the request sent upstream asked 0G for TEE verification.

For buffered inference responses, the gateway accepts only `2xx` responses with
both provider-reported markers present:

- `ZG-Res-Key` exists and is non-empty.
- The JSON response body contains `x_0g_trace.tee_verified` as the JSON boolean
  `true`.

Other statuses and invalid successful responses are rejected before the gateway
returns the body or issues a receipt.

## Receipt Evidence

On an accepted buffered response, the signed `upstream.verified` event records
compact request-specific evidence under
`provider_claims.response_evidence.0g_response_verification`:

- `verification_type: "provider_reported_per_response"`
- `tee_verified: true`
- `zg_res_key_present: true`
- `zg_res_key_sha256`
- `x_0g_trace_sha256`
- `forwarded_body_hash`
- `response_body_hash`
- `cryptographic_binding: "pending_0g_clarification"`

The gateway stores hashes of the response key and trace rather than claiming it has
verified a cryptographic TEE signature. The `upstream.verified.verifier_id` is
`0g/provider-reported-response/v1` to make that scope explicit.

## What Is Not Claimed

This adapter does **not** claim that the gateway cryptographically validates a
TEE quote, a provider TEE signature, or a binding between `ZG-Res-Key`,
`x_0g_trace`, the forwarded request bytes, and the returned response bytes.

0G's public router API points clients to `ZG-Res-Key` and SDK-level verification
for independent proof. The exact cryptographic binding expected at the gateway
layer is pending clarification from 0G, so this first adapter records only
provider-reported per-response verification.

## Streaming Limitation

Streaming 0G requests fail closed with an upstream-verification error:

```text
0G response verification for streaming responses is unsupported
```

The gateway does this before opening the upstream stream. Releasing SSE chunks
before the final `ZG-Res-Key` / `x_0g_trace` response verification would expose
unverifiable bytes, so streaming stays disabled for `provider: "0g"` until the
0G contract provides a verification scheme suitable for incremental responses.

## Configuration

```json
[
  {
    "name": "zero-g-router",
    "provider": "0g",
    "base_url": "https://router-api.0g.ai",
    "models": {
      "0g-model": "<0g-provider-model-id>"
    },
    "bearer_token": "<0g-api-key>"
  }
]
```

`base_url` is the origin. The gateway appends `/v1/chat/completions` for chat
requests unless a route-specific `path` is configured. Production 0G origins
must use HTTPS, and the adapter rejects redirects so evidence stays tied to the
configured origin.
