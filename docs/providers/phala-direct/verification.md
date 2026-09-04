# Phala Direct Verification

The `phala-direct` adapter connects to one dstack-vllm-proxy origin and verifies that origin's legacy version 2 attestation report. It is a per-model compatibility path for services that do not expose canonical ACI reports.

| Property | Current behavior |
| --- | --- |
| Attestation scope | Per model |
| Verifier | `scripts/provider_verifier/phala_direct.py` through the provider-verifier bridge |
| External dependency | dstack verifier at `DSTACK_VERIFIER_URL` |
| Enforced binding | `tls_spki_sha256` |
| Evidence endpoint | `<base_url>/v1/attestation/report?version=2&signing_algo=ecdsa&nonce=<random>` |

## Producer requirement

The upstream proxy must serve attestation version 2 with a custom-domain TLS SPKI bound into TDX report data:

```text
report_data[0:32] = SHA256(signing_address || tls_cert_fingerprint)
report_data[32:64] = request nonce
```

It must also return `tls_cert_fingerprint`. A proxy that ignores `version=2` cannot produce an enforceable custom-domain binding and is rejected.

This property is meaningful only when TLS private-key custody belongs to the attested workload, such as a dstack-ingress sidecar inside the CVM. An off-TEE TLS terminator breaks the intended custody claim even if the digest appears in the report.

## Verification algorithm

For each verification, the adapter:

1. Generates a 32-byte nonce and fetches the version 2 report, using the configured provider credential when present.
2. Requires a TDX quote, signing address, TLS fingerprint, and a matching echoed nonce when the report includes one.
3. Rejects the TDX debug attribute.
4. Sends the quote, event log, and VM configuration to the configured dstack verifier and requires `is_valid: true`.
5. Requires both `app_compose` and `compose_hash`, then checks `SHA256(UTF8(app_compose))`.
6. Parses `report_data` from the verified quote and checks the signing-address, TLS-fingerprint, and nonce layout above.
7. Resolves the attested dstack OS-image hash to published image metadata when possible and records whether it is a development image.
8. Sends available NVIDIA evidence to NRAS and records the nonce-matched result as supplemental metadata.
9. Emits the report's TLS fingerprint as the channel binding.

Steps 1 through 6 and 9 are mandatory. OS classification and GPU verification do not gate the provider result.

## Channel binding and forwarding

The verified TDX quote binds the signing address, fresh nonce, and custom-domain TLS SPKI. The gateway compares that digest with the live HTTPS certificate before forwarding. A missing fingerprint, wrong nonce, changed fingerprint, invalid quote, missing compose input, or compose mismatch fails before prompt forwarding on a constrained request.

Use one upstream entry per distinct origin. Several public aliases can share an entry only when they use the same `base_url`.

## Session claims

| Claim | Mapping |
| --- | --- |
| `tee_attested` | Asserted from the verified TDX quote and bound TLS channel. |
| `tcb_up_to_date` | Asserted only for `UpToDate`; another status is refuted; absence is unknown. |
| `os_known_good` | Asserted for a resolved production dstack image, refuted for a resolved development image, unknown when resolution fails. |
| `gpu_attested` | Asserted as verifier-derived only when NRAS succeeds and the GPU nonce matches. This does not prove that GPU served this request or is bound to the CPU TEE. |
| `serving_software_known_good` | Unknown. Compose integrity is checked, but no reviewed digest allowlist is applied. |
| `model_weights_provenance` | Unknown. |

## Limitations

- A non-current TCB state is recorded rather than rejected by the bridge.
- Production-versus-development OS classification is recorded rather than enforced. Current policy can therefore verify a channel whose session refutes `os_known_good`.
- The compose hash proves integrity against the report. The adapter does not compare the compose or image digests with an operator allowlist.
- GPU evidence is an online existence and nonce check. It is not a CPU-to-GPU or request-serving proof.
- Model weights are not measured into the accepted identity.
- The legacy producer and external dstack verifier are additional trust and availability dependencies.

## Tests and reproduction

Run the hermetic bridge test:

```sh
cargo test --test phala_direct_bridge
```

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

The dated [admissions review](review.md) records the earlier policy decision and unresolved strict-release conditions.
