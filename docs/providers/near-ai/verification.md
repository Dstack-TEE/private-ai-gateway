# NEAR AI Verification

The NEAR AI adapter verifies the public cloud gateway as one router-scoped TDX channel. It does not bind a request to the nested model instance that ultimately served it.

| Property | Value |
| --- | --- |
| Attestation scope | Router |
| Verifier | `scripts/provider_verifier/nearai.py` and vendored `NearAICloudVerifier` |
| External dependency | dstack verifier at `DSTACK_VERIFIER_URL` (default `http://localhost:8080`) |
| Enforced binding | `tls_spki_sha256` |
| Evidence endpoint | `https://cloud-api.near.ai/v1/attestation/report` |

## Verification algorithm

The provider bridge asks NEAR AI for a report with a fresh nonce and `include_tls_fingerprint=true`. `NearAICloudVerifier.verify_gateway_component` then checks the `gateway_attestation` component:

1. Send the TDX quote, event log, and VM configuration to the dstack verifier and require `is_valid: true`.
2. When both `app_compose` and `compose_hash` are present, require `SHA256(UTF8(app_compose))` to equal the reported hash.
3. Require a request nonce and signing address.
4. Parse the 64-byte `report_data` from the same quote bytes the dstack verifier accepted, and verify its layout:

   ```text
   report_data[0:32] = SHA256(signing_address || tls_cert_fingerprint)
   report_data[32:64] = request nonce
   ```

5. When an NVIDIA payload is present, require its nonce to match and require the NVIDIA verifier to succeed.

The bridge then requires a `gateway_attestation`, a `tls_cert_fingerprint`, and a valid component result, and emits one router-scoped TLS SPKI binding. A missing report-data value, nonce, signing address, or TLS fingerprint fails verification.

## Channel binding and forwarding

The accepted TDX quote covers the signing address, nonce, and TLS SPKI digest through `report_data`. The gateway pins that digest against the live NEAR AI HTTPS certificate before sending the request.

The verifier cache omits the model from its key because every configured model uses the same router channel. The receipt records the selected model; the session records the shared router evidence.

These tampered inputs were rejected live against `cloud-api.near.ai`:

| Tampered input | Rejection |
| --- | --- |
| Quote | `Dstack verification failed: Quote verification failed` |
| Request nonce | `Report data check failed: mismatch` |
| `tls_cert_fingerprint` | `Report data check failed: mismatch` |

## Session claims

| Claim | Mapping |
| --- | --- |
| `tee_attested` | Asserted from the verified TDX quote and bound TLS channel. |
| `tcb_up_to_date` | Asserted only for `UpToDate`; another surfaced status is refuted; absence is unknown. |
| `gpu_attested` | Unknown, because the bridge does not emit GPU verdict fields in `provider_claims`. |
| `os_known_good` | Unknown. |
| `serving_software_known_good` | Unknown. |
| `model_weights_provenance` | Unknown. |

## Limitations

- No workload pin. Any dstack CVM that passes the dstack verifier with matching `report_data` is accepted as the NEAR AI gateway. The adapter does not compare the compose, app ID, or image with a reviewed value.
- The bridge verifies the gateway component only. It does not fetch or verify `model_attestations[]` for the request's model.
- Nothing in the emitted session identifies the downstream model CVM that served a response.
- A non-`UpToDate` TCB status can remain `is_valid` and is represented as a refuted typed claim, not a bridge failure.
- Missing compose material is not a failure. A compose that does not match a present hash is rejected, but the adapter does not require both values.
- GPU verification can run when the gateway report includes a payload, but the resulting GPU fields are not preserved in the emitted provider claims.

## Tests and reproduction

Run the hermetic report-data tamper test:

```sh
cargo test --test soundness_report_data
```

For a live verifier run, start a trusted dstack verifier and set `DSTACK_VERIFIER_URL` explicitly:

```sh
export DSTACK_VERIFIER_URL='http://localhost:18080'
request_hash="sha256:$(printf '0%.0s' {1..64})"
jq -n --arg hash "$request_hash" '{
  api_version: "aci.provider-verifier.request.v1",
  provider: "near-ai",
  upstream_name: "near-ai-live",
  url_origin: "https://cloud-api.near.ai",
  model_id: "google/gemma-4-31B-it",
  forwarded_body_hash: $hash,
  required: true,
  timeout_seconds: 300
}' | uv run python scripts/private_ai_provider_verifier.py
```
