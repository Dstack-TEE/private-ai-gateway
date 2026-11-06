# Confidential AI verification

Admission remains **not acceptable**: see [review.md](review.md), previously
[PR #554](https://github.com/Dstack-TEE/private-ai-gateway/pull/554).
Keep this provider out of production config until the policy-swap fix is
reviewed and verified. The adapter does not resolve the operator's ability to
briefly admit an unreviewed worker behind the front door.

## Evidence and channel binding

`provider: confidential-ai` uses `scripts/provider_verifier/c8s.py` through the
standard external bridge, with `PerRouter` scope and a `tls_spki_sha256` binding
enforced by the existing Rust forwarding client. One WebPKI-validated HTTPS
connection captures its serving leaf and fetches, in order:

1. `/.well-known/c8s/attest-lb?nonce=<fresh unpadded base64url 32 bytes>`;
2. `/allowlist`, preserving its raw bytes; and
3. `/.well-known/c8s/routes`, the applied route manifest publication.

Reconnection is disabled. HTTP errors, oversized bodies and non-JSON responses
fail closed. The retired `/attestation` endpoint is never used. The receipt must
echo the nonce, declare TDX and mode `acme`, and identify the observed leaf.
`webpki` and `cds` are rejected. Hardware `report_data` is:

```text
SHA384(LP("c8s/attest-lb/v1") || LP("acme") || LP(nonce) ||
       LP(SHA256(serving_leaf_DER)) || LP(SHA256(mesh_leaf_DER)) ||
       LP(SHA256(mesh_CA_DER)) || LP(route_digest)) || zero[16]
LP(x) = uint32_big_endian(len(x)) || x
route_digest = SHA256(raw route manifest bytes), 32 bytes
```

The mesh leaf signs the 48-byte transcript digest with `ecdsa-sha384`. The
identity proof selects its committed CA by DER SHA-256; the mesh leaf must be
directly issued by that CA, with valid lifetimes and CA constraints. Its single
CA-signed matched-workload stamp (OID `1.3.6.1.4.1.66378.1.5`) must be valid DER,
match the configured workload, and commit to the raw `/allowlist` digest and one
accepted allowlist pin. The workload must occur in that allowlist. The stamp is
CA-vouched, not hardware proof of continuous policy enforcement.

DCAP verifies with online collateral and the pinned `dcap-qvl` dependency. Only
`UpToDate` is accepted; debug TDs are rejected. Verified MRTD and RTMR1/2/3 must
exactly match the configured pins. RTMR0 is recorded and not pinned.

The manifest's exact decoded bytes must hash to the attested `route_digest`,
and the selected host must route `/v1/chat/completions`. For the reviewed baked
node image, the router itself verifies manifests under the operator key from its
signed launch configuration; RTMR3 pins that launch key. This is documented in
c8s v0.80.10's [Route manifest](https://github.com/confidential-dot-ai/C8s/blob/cd68ff62791ffc792ecf45583151a278e7291512/docs/operator.md#route-manifest),
[Authenticated launch configuration](https://github.com/confidential-dot-ai/C8s/blob/cd68ff62791ffc792ecf45583151a278e7291512/docs/operator.md#authenticated-launch-configuration),
and [Policy history](https://github.com/confidential-dot-ai/C8s/blob/cd68ff62791ffc792ecf45583151a278e7291512/docs/measurements.md#policy-history).
[Route evidence](https://github.com/confidential-dot-ai/C8s/blob/cd68ff62791ffc792ecf45583151a278e7291512/docs/routes-evidence.md#what-the-evidence-proves)
confirms invalid write tokens are refused. The client relies on that measured
router behavior instead of duplicating its signature check or configuring a
second copy of the operator key. This reasoning is specific to the baked-node
ACME profile; other deployments can take operator keys from Kubernetes config.

## Required configuration

Use `accepted_subjects`, as for other providers. Require exactly one each of
`tdx-mrtd:<96 lowercase hex>`, `tdx-rtmr1:<96 lowercase hex>`,
`tdx-rtmr2:<96 lowercase hex>`, `tdx-rtmr3:<96 lowercase hex>` and
`c8s-workload:<name>`, plus one or more `c8s-allowlist:sha256:<64 lowercase hex>`
pins. Config load validates their format and presence; the bridge checks them
against authenticated evidence. There is no release registry or built-in pin.

The following is a **test-only** v0.16.5 example (captured 2026-11-05 at 17:13:57 PST):

```json
[
  {
    "name": "confidential-ai-test",
    "provider": "confidential-ai",
    "base_url": "https://api.confidential.ai",
    "models": {
      "deepseek-ai/DeepSeek-V4.1-Flash": "deepseek-ai/DeepSeek-V4.1-Flash"
    },
    "accepted_subjects": [
      "tdx-mrtd:9309eaae9c151e766de0f97b1d1aaeb76b8c8c366080803943fb566521c8f0cf00a142d8b7b0683ed1d42c5a27198ba1",
      "tdx-rtmr1:9301c6e6b644e4dbc013c2bc8bceedd4158438a19ee0dc07b960c9b0bd9357c4b15a15957119397d3cb35c59744399d1",
      "tdx-rtmr2:ec87b28b559c36944c1cab451114370d33c05a5278b7e1ae03b0d9e667ca03cd9d10a6cbd4296ec50d222e3124f6c582",
      "tdx-rtmr3:256e9368dbd9aff2e28044988a756d4554c1ec5880fa4ea829cd5bc854f62842b61ab7c25c55327d048fa5c9e1666ba8",
      "c8s-workload:c8s-router-7210bdeb078b",
      "c8s-allowlist:sha256:bdf3ccceaa4b85cb9b2f10ff15d7c9e3226d226db1759db8eefdcdb1b561e2af"
    ]
  }
]
```

Exposure is controlled by `models`. The example exposes only V4.1-Flash; the
provider's old `DeepSeek-V4-Flash-0731` ID silently aliases it, so use the current
name in config. The verifier imposes no separate model allowlist. Inference
credentials, if required, use the standard `bearer_token` field.

## Cache window and receipt claims

Cache and refresh use the same settings as every other provider: the global
defaults are 300 seconds and 240 seconds. There is a **<=300s post-verification
window with those defaults**; overriding the cache changes that window.
Attestation and inference use separate connections. Forwarding enforces the
cached SPKI pin and does not run attest-lb on the inference connection, so it
does not carry c8s's per-connection route/policy binding. Already admitted streams
can continue after cache expiry. No provider-specific cache cap is imposed.

Typed claims assert the front door's TEE identity at verification time and its
`UpToDate` TCB. Software/OS provenance, model weights, worker identity and GPU
attestation remain unknown. Raw claims explicitly record
`continuous_policy_enforcement=false`, `worker_identity_verified=false`, and
`gpu_verified=false`, with the measurements, workload, allowlist, route
digest/revision and serving leaf/SPKI digests. Evidence contains the consumed
receipt, raw allowlist, routes publication and serving leaf bytes.

## Refreshing pins and testing

Download the selected [release's](https://github.com/confidential-dot-ai/confidential-inference/releases)
`release-bundle.json`, `release-bundle.sigstore.json`, and `allowlist.json`.
Verify before taking pins:

```sh
release=v0.16.5
cosign verify-blob --bundle release-bundle.sigstore.json \
  --certificate-identity "https://github.com/confidential-dot-ai/confidential-inference/.github/workflows/release-bundle.yml@refs/tags/$release" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com release-bundle.json
sha256sum allowlist.json
```

Require `Verified OK`; match the allowlist's exact bytes to the signed bundle
and review policy changes. Take MRTD, RTMR1/2 and allowlist SHA-256 from that
bundle. Review the launch operator key separately and derive RTMR3 as
`SHA384(zero[48] || SHA384(exact launch-key PEM bytes))`; the key is not included
in this release bundle. Review the expected router workload as well. Live
evidence alone is not authority to change pins. The fixture records the
Cosign-verified v0.16.5 bundle digest and public capture/collateral.

```sh
uv run --frozen python tests/provider_verifier/c8s_soundness.py
cargo test --test confidential_ai_bridge
```

Soundness tests verify the captured real quote at capture time and synthetic
signed CA chains for missing/duplicate stamps and other negative cases. Rust
tests cover pin validation and propagation. Fixture checks also reject a
one-nibble change to every measurement, allowlist and workload pin.
The live acceptance rerun on 2026-11-05 at 19:20 PST is blocked by the provider:
attest-lb, routes and allowlist return HTTP 404. Keep these v0.16.5 pins and
examples; this deployment change has not been reviewed. No inference request
or worker/GPU verification is performed.

Protocol references: MIT [TEErminator](https://github.com/confidential-dot-ai/TEErminator)
and [c8s-verify-js](https://github.com/confidential-dot-ai/c8s-verify-js).
The verifier ports this repository's prior Python code. AGPL-3.0 c8s
documentation is used for protocol facts only; no c8s implementation code is copied.
