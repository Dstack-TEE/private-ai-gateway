# Privatemode Active Secret Evidence

> **Status:** draft provider extension to `aci/1`
> **Binding:** [`proxy_image_sha256`](proxy-image-binding.md)

This extension binds a response's inference secret to its Contrast manifest
through provider claims and session evidence. Core ACI schemas are unchanged.

## Proxy contract

The fork exposes this header and API only with `--expose-secret-evidence`
(default `false`, preserving upstream v1.60.0 behavior).

The measured proxy MUST set `Privatemode-Secret-ID` before every proxied
inference response body, including streaming. It contains standard padded
base64 of `SHA256(request_public_key)` for the attempt producing that response.

`GET /privatemode/v1/secret-evidence/{id}` at the measured proxy origin uses
unpadded base64url of the same 32 bytes. It returns `200 application/json`,
`404` for unknown/evicted IDs, or `400` for malformed IDs. Evidence MUST remain
associated with the requested secret across rotation; the gateway never selects
evidence through a `current` endpoint.

Version 1 JSON (all byte fields standard padded base64; times RFC 3339 UTC):

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

Bundles MUST NOT contain the inference secret, API key, or prompt data.

## Gateway requirements

`privatemode_proxy.active_secret_evidence` is static, measured, and defaults to
`false`. Omission or `false` preserves existing behavior, claims, and wire output.
Enabling it requires a proxy implementing this contract with
`--expose-secret-evidence`. If the proxy flag is off, every 2xx response fails
closed for missing `Privatemode-Secret-ID`.

The gateway MUST enforce readiness and the existing proxy-image binding before
forwarding. Enabled mode does not read manifest logs. For every 2xx response it
MUST validate the header and obtain matching evidence before releasing provider
bytes; `aci_verified` streaming responses MUST also remain uncommitted. Client
session constraints apply before forwarding and again after response binding.
Non-2xx responses bypass binding, retain normal status/error, capacity,
`Retry-After`, and failover handling, and cite no active-secret session.

Evidence fetches use only the measured proxy origin, reject redirects and
ambient proxies, and have a total deadline of
`min(10, verifier_request_timeout_seconds)` seconds, including cache waits and
body reads. The cache retains at most eight immutable bundles; failures are not
cached. Fetches do not hold its mutex; concurrent misses may fetch the same ID.

Validation rejects missing/unknown fields, versions other than integer `1`,
noncanonical encodings, non-object manifests, invalid PEM/timestamps, unordered
times, and size violations. Header, URL, bundle ID, and
`SHA256(request_public_key)` MUST agree; the decoded manifest MUST match its
64-character lowercase hexadecimal digest. All fields are nonempty. Limits:

| Field | Maximum decoded size |
| --- | --- |
| Whole JSON body | 8 MiB (wire bytes) |
| Manifest | 4 MiB |
| Attestation document | Bounded by the whole JSON body, including base64 encoding |
| Secret Service certificate | 64 KiB |
| Mesh CA PEM | 64 KiB, 16 certificates |
| Request public key, encapsulated key | 4 KiB each |
| Nonce, signature | 1 KiB each |
| Secret ID | Exactly 32 bytes |

Missing, invalid, or mismatched evidence MUST produce the existing signed
`503 upstream_verification_failed` refusal, intentionally even without an
`aci_verified` constraint. Session-pin mismatches use `412 session_not_accepted`.
No provider bytes are released. When the prompt already reached the proxy, the
refusal records `request.forwarded` with its exact body hash before failed
`upstream.verified`. Pre-forward refusals retain their existing event sequence.

## Sessions and verification

Successful sessions include:

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

The receipt's `upstream.verified.session_id` cites this immutable session;
secret ID and evidence digest participate in its identity. No bound session is
listed before a valid response. Discovery then offers the most recently
observed secret under the readiness lease, without promising the next request
will use it. Existing trust/encryption claims are preserved; GPU, TCB, OS,
serving-software, and model-weight claims remain `unknown`.

The gateway checks shape and consistency and trusts the measured proxy's
cryptographic checks. Decode `evidence.data`, verify its digest, then use the
[proxy fork's verifier](https://github.com/redpill-ai/privatemode-public/pull/1):

```sh
verify-evidence --evidence evidence.json --accept-manifest-sha256 <hex>
```

It checks Contrast attestation/nonce, the single manifest and Mesh CA, the
Secret Service certificate chain/signature over
`SHA256(request_public_key || encapsulated_key)`, and secret ID. Success exits
zero and prints the verified manifest hash. It fetches AMD/Intel collateral
online; recorded-evidence verification is not air-gapped.

`manifest_bound_to_active_secret: true` and `active_manifest_sha256` identify
which manifest the proxy's Coordinator attested for that secret. Trust requires
comparing that hash with Privatemode's published CDN/release manifest or the
relying party's reviewed allowlist. Repeat `--accept-manifest-sha256` for multiple
trusted hashes. Without an allowlist, PASS proves only internal consistency.

[Hermetic tests](../tests/provider_e2e/privatemode_active_secret.rs) use synthetic
attestation bytes; independent cryptographic verification and live CVM
acceptance remain separate checks.
