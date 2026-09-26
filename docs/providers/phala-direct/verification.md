# Phala Direct Verification

The `phala-direct` adapter connects to one dstack-vllm-proxy origin and verifies that origin's legacy version 2 attestation report. It is a per-model compatibility path for services that do not expose canonical ACI reports.

| Property | Value |
| --- | --- |
| Attestation scope | Per model |
| Verifier | `scripts/provider_verifier/phala_direct.py` through the provider-verifier bridge |
| External dependency | dstack verifier at `DSTACK_VERIFIER_URL` (default `http://localhost:8080`) |
| Enforced binding | `tls_spki_sha256` |
| Evidence endpoint | `<base_url>/v1/attestation/report?version=2&signing_algo=ecdsa&nonce=<random>` |

## Producer requirement

The upstream proxy must serve attestation version 2 with a custom-domain TLS SPKI bound into TDX report data:

```text
report_data[0:32] = SHA256(signing_address || tls_cert_fingerprint)
report_data[32:64] = request nonce
```

`tls_cert_fingerprint` is the SHA-256 of the leaf certificate's SubjectPublicKeyInfo DER, and the report must return it. A proxy that ignores `version=2` cannot produce an enforceable custom-domain binding and is rejected.

This property is meaningful only when TLS private-key custody belongs to the attested workload, such as a dstack-ingress sidecar inside the CVM. An off-TEE TLS terminator breaks the intended custody claim even if the digest appears in the report.

## Verification algorithm

For each verification, the adapter:

1. Generates a 32-byte nonce and fetches the version 2 report, using the configured provider credential when present.
2. Requires a TDX quote, signing address, TLS fingerprint, and a matching echoed nonce when the report includes one.
3. Rejects a debug TD: the TUD byte of `TD_ATTRIBUTES` (quote offset 168) must be zero.
4. Sends the quote, event log, and VM configuration to the dstack verifier and requires `is_valid: true`. The dstack verifier replays the event log against the quote's RTMRs.
5. Requires `app_compose` and checks that `SHA256(UTF8(app_compose))` equals the `compose-hash` event in the verified event log.
6. Parses `report_data` from the verified quote and checks the signing-address, TLS-fingerprint, and nonce layout above.
7. Sends available NVIDIA evidence to NRAS and records the nonce-matched result as supplemental metadata.
8. Resolves the attested dstack OS-image hash as described in [How the OS image is classified](#how-the-os-image-is-classified).
9. Emits the report's TLS fingerprint as the channel binding.

Steps 1 through 6 and 9 gate the result. GPU verification and OS classification are recorded but never reject.

## Channel binding and forwarding

The verified TDX quote binds the signing address, fresh nonce, and custom-domain TLS SPKI. The backend's `SpkiPinVerifier` compares that digest with the live HTTPS certificate's SPKI before forwarding. A missing fingerprint, wrong nonce, changed fingerprint, invalid quote, missing `app_compose`, or a compose that does not match the measured `compose-hash` event fails before prompt forwarding on a constrained request.

Use one upstream entry per distinct origin. Several public aliases can share an entry only when they use the same `base_url`.

## Provider claims recorded

The attested-session layer reads these `provider_claims` keys, so their names and meanings are a stable contract.

| Key | Value |
| --- | --- |
| `trust_boundary` | `phala-dstack-cvm` |
| `evidence_scope` | `model_instance` |
| `canonical_model_id` | The configured model ID. |
| `attestation_version` | `2` |
| `tls_spki_from_report_data` | `true` |
| `signing_address` | The response-signing address bound into `report_data`. |
| `report_data_nonce_matched` | `true` |
| `compose_hash_verified` | `true` |
| `tdx_debug_mode` | `false`, because debug TDs are rejected. |
| `tcb_status` | The dstack verifier's TCB status, such as `UpToDate`. |
| `os_image_hash` | The attested dstack OS-image hash. |
| `os_image_version` | The resolved image version, or `null`. |
| `os_image_is_dev` | The resolved `is_dev` flag, or `null`. |
| `production_os_image` | `true` for a production image, `false` for a development image, `null` when the hash cannot be resolved. |
| `gpu_verified` | `true` only when NRAS accepts the evidence and the GPU nonce matches. |
| `gpu_evidence_present` | Whether the report carried NVIDIA evidence. |
| `gpu_evidence_nonce_matched` | Whether the GPU evidence nonce equals the request nonce, or `null` without evidence. |
| `gpu_arch` | The GPU architecture from the evidence. |

## Session claims

| Claim | Mapping |
| --- | --- |
| `tee_attested` | Asserted from the verified TDX quote and bound TLS channel. |
| `tcb_up_to_date` | Asserted only for `UpToDate`; another status is refuted; absence is unknown. |
| `os_known_good` | Asserted for a resolved production dstack image, refuted for a resolved development image, unknown when resolution fails. |
| `gpu_attested` | Asserted as verifier-derived only when NRAS succeeds and the GPU nonce matches. This does not prove that GPU served this request or is bound to the CPU TEE. |
| `serving_software_known_good` | Unknown. Compose integrity is checked, but no reviewed digest allowlist is applied. |
| `model_weights_provenance` | Unknown. |

## How the OS image is classified

The classification is bound to the attestation, so the image download server cannot change it:

1. The dstack verifier returns `app_info.os_image_hash` and reports `is_valid` only after it reproduces MRTD and the RTMRs from that exact image.
2. dstack defines `os_image_hash` as `SHA256(sha256sum.txt)`, and that manifest pins `SHA256(metadata.json)`. Flipping the `is_dev` flag in `metadata.json` would change the manifest and therefore the attested hash.
3. `resolve_os_image` in `scripts/dstack_os_image.py` looks the hash up in the reviewed `KNOWN_OS_IMAGES` map, then in an on-disk cache, and otherwise downloads `https://download.dstack.org/os-images/mr_<os_image_hash>.tar.gz`. It checks both digest equalities before it reads `is_dev`.
4. `production_os_image` is `not is_dev`. A hash that cannot be resolved stays `null`; it is never treated as production.

Check an image, or produce a new `KNOWN_OS_IMAGES` entry, with:

```sh
uv run python scripts/dstack_os_image.py <os_image_hash>
```

The adapter records the classification and does not enforce it. A relying party that requires a production image must apply a claims policy.

## Limitations

- A non-current TCB state is recorded rather than rejected by the bridge.
- Production-versus-development OS classification is recorded rather than enforced, so a verified channel can carry a session that refutes `os_known_good`.
- The compose check proves the compose was measured. The adapter does not compare the compose or image digests with an operator allowlist.
- GPU evidence is an online existence and nonce check. It is not hardware-bound to the serving CPU TEE and does not prove which GPU served a request.
- Model weights are not measured into the accepted identity.
- The legacy producer and external dstack verifier are additional trust and availability dependencies.

## Tests and reproduction

Run the hermetic bridge test:

```sh
cargo test --test phala_direct_bridge
```

It runs `scripts/soundness_phala_direct.py`, which stubs the HTTP fetch, dstack verifier, and NRAS but uses the real `report_data` logic. The script checks these outcomes:

| Input | Outcome |
| --- | --- |
| Genuine version 2 report | Verified with the `tls_spki_sha256` binding and the claims above. |
| Missing `tls_cert_fingerprint` | Rejected. |
| Fingerprint that differs from the one bound in `report_data` | Rejected as a `report_data` binding failure. |
| Debug TD | Rejected. |
| dstack verifier returns `is_valid: false` | Rejected. |
| GPU nonce mismatch | Verified with `gpu_evidence_nonce_matched: false` and `gpu_verified: false`. |
| NRAS failure | Verified with `gpu_verified: false`. |
| Unresolvable OS image | Verified with `production_os_image: null`. |
| OS-image archive with a flipped `is_dev` or a wrong hash | Rejected by `resolve_os_image`. |

`tests/soundness_report_data.rs` covers the shared `report_data` binding logic.

For a live endpoint, start a trusted dstack verifier and set the optional provider credential:

```sh
export DSTACK_VERIFIER_URL='http://localhost:8080'
export PHALA_DIRECT_API_KEY='...'
request_hash="sha256:$(printf '0%.0s' {1..64})"
jq -n \
  --arg origin 'https://model.example' \
  --arg model 'provider/model-id' \
  --arg key "$PHALA_DIRECT_API_KEY" \
  --arg hash "$request_hash" \
  '{
    api_version: "aci.provider-verifier.request.v1",
    provider: "phala-direct",
    upstream_name: "phala-direct-live",
    url_origin: $origin,
    model_id: $model,
    forwarded_body_hash: $hash,
    required: true,
    timeout_seconds: 300,
    provider_options: {phala_direct_bearer_token: $key}
  }' | uv run python scripts/private_ai_provider_verifier.py
```

The dated [admissions review](review.md) lists the conditions for strict-release inclusion.
