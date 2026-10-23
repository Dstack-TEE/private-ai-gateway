# Privatemode Active Secret Evidence

> **Status:** draft provider extension to `aci/1`
> **Binding:** [`proxy_image_sha256`](proxy-image-binding.md)

This extension records the Contrast manifest associated with the inference
secret used for a particular response. It adds provider fields in
`claims.extra` and a session evidence format; it does not change the core
session, receipt, or channel-binding schema.

## Proxy contract

The measured proxy MUST set `Privatemode-Secret-ID` before the body of every
proxied inference response, including streaming responses. Its value is the
standard padded base64 encoding of `SHA256(request_public_key)` for the attempt
that produced that response.

The gateway obtains the bundle with
`GET /privatemode/v1/secret-evidence/{id}` at the measured proxy origin. The URL
ID is the unpadded base64url encoding of those same 32 bytes. The endpoint
returns `200 application/json`, `404` for unknown or evicted IDs, or `400` for a
malformed ID. It MUST return the requested secret's evidence even when the
proxy rotates its current secret during the request. The gateway never uses a
`current` endpoint to select receipt evidence.

The version 1 JSON shape is:

```json
{
  "version": 1,
  "secret_id": "<std b64>",
  "manifest_sha256": "<lowercase hex>",
  "manifest": "<b64 bytes>",
  "coordinator_attestation": {
    "nonce": "<b64>",
    "document": "<b64 attestation doc as sent to ValidateAttestation>",
    "attested_at": "<time>"
  },
  "mesh_ca_pem": "<PEM string>",
  "secret_service_cert": "<b64 DER>",
  "request_public_key": "<b64>",
  "encapsulated_key": "<b64>",
  "signature": "<b64 ASN.1 ECDSA>",
  "exchanged_at": "<time>",
  "expires_at": "<time>"
}
```

All byte fields use standard base64 with padding. Timestamps are RFC 3339 UTC.
The bundle MUST NOT contain the inference secret, API key, or prompt data.

## Gateway requirements

`privatemode_proxy.active_secret_evidence` is a static, measured boolean,
`false` by default. Omission or `false` preserves the existing manifest
observation, session claims, forwarding behavior, and wire format. Enabling it
requires a proxy image that implements this contract.

With the switch enabled, the gateway MUST:

1. Verify deployment readiness and enforce the existing proxy-image binding
   before forwarding. A readiness response alone MUST NOT publish an active
   secret session. Manifest logs are not required in this mode.
2. For every 2xx response, read the response header, validate its canonical
   encoding and length, and fetch evidence for exactly that ID from the fixed
   proxy origin. Reject redirects and ambient HTTP proxies.
3. Reject missing or unknown fields, versions other than integer `1`, invalid
   encodings, non-object manifests, invalid PEM framing, invalid or unordered
   timestamps, and size-limit violations. Require the response ID, bundle ID,
   URL ID, and `SHA256(request_public_key)` to agree, and check the digest of
   the decoded manifest.
4. Bind the per-request recorded event to this bundle before releasing a
   provider body. For `aci_verified` requests, this MUST precede committing a
   streaming response. Recheck any client session
   allowlist against that event. A rotated secret can produce a new session;
   it cannot reuse the previous secret's session in the receipt.
5. Fail closed with the existing verification refusal and signed refusal
   receipt on missing headers or missing, invalid, or mismatched evidence.
   Session-allowlist mismatches use the existing `412 session_not_accepted`.
   Other evidence failures use `503 upstream_verification_failed`. No provider
   body bytes are released. Verification can fail after the request reached
   the proxy. Such refusals record `request.forwarded` with the exact forwarded
   body hash before the failed `upstream.verified` event. Pre-forward refusals
   retain their existing event sequence without `request.forwarded`.

With the switch enabled, evidence failures intentionally refuse requests even
when the client did not require `aci_verified`. This stricter fail-closed
behavior applies to all 2xx responses. Non-2xx responses bypass response binding
and retain the ordinary upstream-status, error-normalization, capacity-retry,
`Retry-After`, and failover handling. Their receipts do not cite an active-secret
session because no successful response was bound.

The evidence fetch has a total timeout of the smaller of ten seconds and the
route's `verifier_request_timeout_seconds`, including cache-lock waits and body
reads. Network fetches do not hold the cache mutex, so a slow ID does not block
other cache lookups or fetches. Concurrent misses for one ID may fetch twice;
the cache retains a single immutable bundle for that ID. The gateway accepts at
most 8 MiB of JSON, with decoded field limits of 4 MiB for the manifest, 1 MiB
for the attestation document, 64 KiB for the Secret Service
certificate, 4 KiB each for the request public key and encapsulated key, and
1 KiB each for the nonce and signature. The PEM string is limited to 64 KiB
and 16 certificates. Fields must be nonempty; the secret ID is exactly 32
bytes and the manifest digest is exactly 64 lowercase hexadecimal characters.
The immutable evidence cache retains at most eight IDs per verifier. Failed
fetches and invalid bundles are not cached.

## Session and receipt

A successfully bound session includes:

```json
{
  "claims": {
    "extra": {
      "active_secret_evidence": true,
      "manifest_bound_to_active_secret": true,
      "active_manifest_sha256": "<64 lowercase hex>",
      "secret_id": "<std b64>"
    }
  },
  "evidence": {
    "digest": "sha256:<SHA256 of exact evidence JSON bytes>",
    "data": "data:application/json;base64,<exact evidence JSON bytes>"
  }
}
```

The existing trust-boundary, request-encryption, response-authentication,
attestation-scope, and inference-secret-policy claims are preserved. Typed GPU,
TCB, OS, serving-software, and model-weight claims remain `unknown`. The
gateway trusts the measured proxy's attestation checks and does not verify
Contrast or TEE evidence itself, or decide whether a manifest is acceptable.

`manifest_bound_to_active_secret: true` together with `active_manifest_sha256`
identifies which manifest the proxy's Coordinator attested for that secret.
Trust in that manifest requires comparing `active_manifest_sha256` against a
trusted manifest: Privatemode's published CDN or release manifest, or the
relying party's reviewed allowlist. These claims alone do not approve the
manifest's contents.

The receipt's existing `upstream.verified.session_id` cites this immutable
session. The secret ID and evidence digest participate in the session material,
so another secret cannot reuse the session. The header itself adds no receipt
field. The gateway only lists bound sessions after it has observed a valid
response; the most recently observed session is a discovery hint, not a promise
that a subsequent request will use the same secret. Client session constraints
are checked before forwarding and again before releasing the response.

## Offline verification

Decode the session's `evidence.data` and verify its digest, then run the proxy
fork's independent verifier:

```sh
verify-evidence --evidence evidence.json \
  --accept-manifest-sha256 <reviewed-manifest-sha256>
```

Repeated `--accept-manifest-sha256` flags allow several reviewed manifests. The
verifier checks Contrast attestation with the recorded nonce, the single
manifest and attested Mesh CA, the Secret Service certificate chain and
signature over `SHA256(request_public_key || encapsulated_key)`, and the secret
ID. Success exits zero and prints the verified manifest SHA-256. Gateway shape
checks alone are not independent cryptographic verification.

Use `--accept-manifest-sha256 <hex>` to enforce the trusted manifest comparison.
Without an allowlist, a PASS proves only internal consistency of the supplied
bundle. The verifier fetches AMD/Intel collateral online, so verification of a
recorded bundle is not air-gapped.

The hermetic fixtures in
[`tests/provider_e2e/privatemode_active_secret.rs`](../tests/provider_e2e/privatemode_active_secret.rs)
cover accepted and rejected bundles, rotation, session constraints, cache
retention, redirects, size limits, and streaming deadlines. They deliberately
use synthetic attestation bytes: the proxy verifier and live CVM acceptance
are separate checks.
