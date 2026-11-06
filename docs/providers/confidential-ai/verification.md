# Confidential AI verification

Admission remains **not acceptable**: see the
[round-4 admission review](review.md#round-4-2026-11-05-pst).
Do not add this provider to production configuration. Implementing a verifier
does not resolve the operator's ability to change CDS policy briefly and admit
an unreviewed worker behind the front door. Re-review the provider's fix before
enabling traffic.

## Evidence and channel binding

`provider: confidential-ai` uses `scripts/provider_verifier/c8s.py` through the
standard external bridge, with `PerRouter` scope. One WebPKI-validated HTTPS
connection fetches, in order:

1. `/.well-known/c8s/attest-lb?nonce=<fresh unpadded base64url 32 bytes>`;
2. `/allowlist`, preserving the raw bytes; and
3. `/.well-known/c8s/routes`, the operator-signed manifest publication.

The serving leaf is captured from that same connection. Automatic reconnection
is disabled; HTTP errors, oversized bodies and non-JSON responses fail closed.
The retired `/attestation` metadata endpoint is never used.

The `c8s/attest-lb/v1` receipt must echo the nonce, declare TDX and mode `acme`,
and identify the observed serving leaf. `webpki` and `cds` are rejected.
Hardware `report_data` is:

```text
SHA384(LP("c8s/attest-lb/v1") || LP("acme") || LP(nonce) ||
       LP(SHA256(serving_leaf_DER)) || LP(SHA256(mesh_leaf_DER)) ||
       LP(SHA256(mesh_CA_DER)) || LP(route_digest)) || zero[16]
LP(x) = uint32_big_endian(len(x)) || x
route_digest = SHA256(raw route manifest bytes), 32 bytes
```

The mesh leaf signs the 48-byte transcript digest with `ecdsa-sha384`. The
identity proof selects its committed CA by DER SHA-256; the mesh leaf must be
directly issued by that CA, with valid lifetimes and CA constraints. The
CA-signed matched-workload stamp (OID `1.3.6.1.4.1.66378.1.5`) must occur exactly
once, be valid DER, match the configured workload, and commit to both the raw
`/allowlist` digest and one configured accepted digest. That workload must exist
in the allowlist. The stamp is CA-vouched, not a hardware assertion of policy
enforcement.

The routes publication contains base64 manifest bytes and an ES256 stored PUT
authorization. Its signature must verify under `operator_key_pem`; `htm=PUT`,
`htu=/routes`, `pbh=base64url(SHA256(manifest))`, and `0 < exp-iat <= 300`
are mandatory. As in the reference clients, expiry bounds the original write
authorization, not how long the applied manifest remains valid. Freshness comes
from the nonce-bound attested digest. The manifest digest must equal the seventh
transcript field, and the selected host must route `/v1/chat/completions`.
No route rollback floor is persisted across verifier invocations.

DCAP uses the pinned `dcap-qvl` dependency, online collateral from its Phala
PCCS, and current-time verification to Intel's root. Only `UpToDate` is accepted;
debug TDs are rejected. Verified MRTD, RTMR1, RTMR2 and RTMR3 must exactly match
configuration. RTMR3 must also equal the operator-key seed:

```text
RTMR3 = SHA384(zero[48] || SHA384(exact operator_key_pem ASCII bytes))
```

RTMR0 is recorded for diagnostics and is not pinned. GPU attestation and worker
identity are not established by this protocol.

## Required configuration

All `confidential_ai` fields below are required; empty/malformed pins and an
RTMR3/operator-key seed mismatch are rejected at config load. Measurement pins
are 96 lowercase hex characters; allowlist entries are `sha256:` plus 64
lowercase hex characters. The operator public key must be P-256 for ES256 routes.
`release` is an operator label, not runtime release discovery. No release registry
or built-in production pins exist.

Only `deepseek-ai/DeepSeek-V4.1-Flash` is accepted. Every upstream model in
`models` must belong to `accepted_models`; the silent legacy alias
`DeepSeek-V4-Flash-0731` is rejected.

The following is a **test-only** v0.16.5 example, captured on 2026-11-05 at 17:13:57 PST:

```json
[
  {
    "name": "confidential-ai-test",
    "provider": "confidential-ai",
    "base_url": "https://api.confidential.ai",
    "models": {
      "deepseek-ai/DeepSeek-V4.1-Flash": "deepseek-ai/DeepSeek-V4.1-Flash"
    },
    "confidential_ai": {
      "mrtd": "9309eaae9c151e766de0f97b1d1aaeb76b8c8c366080803943fb566521c8f0cf00a142d8b7b0683ed1d42c5a27198ba1",
      "rtmr1": "9301c6e6b644e4dbc013c2bc8bceedd4158438a19ee0dc07b960c9b0bd9357c4b15a15957119397d3cb35c59744399d1",
      "rtmr2": "ec87b28b559c36944c1cab451114370d33c05a5278b7e1ae03b0d9e667ca03cd9d10a6cbd4296ec50d222e3124f6c582",
      "release": "v0.16.5",
      "rtmr3": "256e9368dbd9aff2e28044988a756d4554c1ec5880fa4ea829cd5bc854f62842b61ab7c25c55327d048fa5c9e1666ba8",
      "operator_key_pem": "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEdd2OvT2CGea9fPJpD6OZhyT4F7gL\nuz4qupEMI/HNXmLhcT5o/P64an4pnEyRz1SQ///59EXsSKIE67/twmhCDw==\n-----END PUBLIC KEY-----\n",
      "workload": "c8s-router-7210bdeb078b",
      "allowlist_sha256": [
        "sha256:bdf3ccceaa4b85cb9b2f10ff15d7c9e3226d226db1759db8eefdcdb1b561e2af"
      ],
      "accepted_models": [
        "deepseek-ai/DeepSeek-V4.1-Flash"
      ]
    }
  }
]
```

Bearer credentials, if needed for inference, use the existing `bearer_token`
field and are not required by the verification endpoints. Public config snapshots
expose these public pins, never the bearer credential.

## Cache window and receipt claims

A successful result returns `tls_spki_sha256`; the existing Rust
`pinned_spki_client` enforces that key on forwarding TLS connections. The standard
verifier cache lasts 300 seconds with a 240-second background refresh by default.
Config may shorten the cache but cannot extend it beyond 300 seconds; refresh
intervals above 240 seconds are rejected. Setting refresh to zero disables
background refresh, while cache expiry still forces request-time verification.

**There is a <=300s post-verification window.** Attestation runs on a separate
connection from inference. An SPKI-pinned inference connection does not run
`attest-lb`, and this adapter does not carry c8s's per-connection route/policy
binding onto that connection. A provider-side policy or route change after the
check is not detected until another verification; a check never establishes
continuous policy enforcement. A stream already admitted is not terminated when
the cached verdict expires.

Typed claims assert only the front door's TEE identity at verification time and
its `UpToDate` TCB. Serving-software provenance, OS provenance, model weights,
worker identity and GPU attestation remain unknown. Raw claims explicitly record
`continuous_policy_enforcement=false`, `worker_identity_verified=false`, and
`gpu_verified=false`, alongside the release label, measurements, workload,
allowlist digest, route digest/revision, serving leaf and SPKI digests. Evidence
contains the exact receipt, allowlist, routes publication and serving leaf bytes.

## Refreshing pins

Download `release-bundle.json`, `release-bundle.sigstore.json`, and
`allowlist.json` from the selected
[Confidential Inference release](https://github.com/confidential-dot-ai/confidential-inference/releases).
Verify the bundle before taking measurement or allowlist pins from it:

```sh
release=v0.16.5
cosign verify-blob --bundle release-bundle.sigstore.json \
  --certificate-identity "https://github.com/confidential-dot-ai/confidential-inference/.github/workflows/release-bundle.yml@refs/tags/$release" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com release-bundle.json
sha256sum allowlist.json
```

Require `Verified OK`, match the allowlist's exact byte digest against the signed
bundle, and review policy changes. Copy `mrtd`, `rtmr1`, `rtmr2`, and
`allowlist.sha256` into config. Review the operator key separately: it is not
included in this signed release bundle. The reviewed
[v0.16.5 operator key](https://github.com/confidential-dot-ai/confidential-inference/blob/v0.16.5/docs/operator-keys/cea9208dbb615c4e.pem)
has SPKI SHA-256
`cea9208dbb615c4ea6f136572a3bc80087d9fb597bfa683ee97c88bd249ceb02`.
Preserve the PEM's exact bytes, derive RTMR3 with the formula above, and review
the expected router workload. Live evidence alone is not authority to change
pins. Update `accepted_models` only with a reviewed adapter/spec change.

The v0.16.5 bundle was verified with Cosign v3.1.4, using the workflow identity
above. Its raw SHA-256 is `2cd8d47f2dc29a9a837d294c0e508909431751e23a43420a91fc104b57871eaf`.

## Verification and sources

`tests/fixtures/c8s/v0.16.5.json` contains public live evidence, the served leaf,
Intel collateral, capture time, and test pins. Hermetic tests verify the real
quote/collateral at capture time and use synthetic signed CA chains for stamp
edge cases, including missing/duplicate stamps. Run:

```sh
uv run --frozen python tests/provider_verifier/c8s_soundness.py
cargo test --test confidential_ai_bridge
```

Additional Rust tests check typed config rejection, option propagation, router
scope, conservative claims, and a forwarding SPKI pin rejecting another key.
The acceptance run verified the live origin with v0.16.5 pins, then rejected a
one-nibble change to each measurement and allowlist pin, a public-key nibble
change, and a one-character workload change. No inference request or worker/GPU
verification was performed.

Protocol references are [TEErminator](https://github.com/confidential-dot-ai/TEErminator)
(`internal/verifier/endpoint.go`, `workloadext.go`) and
[c8s-verify-js](https://github.com/confidential-dot-ai/c8s-verify-js)
(`src/routes.ts`), both MIT-licensed. The implementation ports this repository's
prior Python verifier; it does not copy c8s implementation code. The
[AGPL-3.0 c8s documentation](https://github.com/confidential-dot-ai/C8s/tree/v0.80.10/docs)
is a protocol reference only.
