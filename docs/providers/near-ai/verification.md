# NEAR AI Verification

The NEAR AI adapter verifies the public cloud gateway as one router-scoped TDX channel. It does not bind a request to the nested model instance that ultimately served it.

| Property | Current behavior |
| --- | --- |
| Attestation scope | Router |
| Verifier | `scripts/provider_verifier/nearai.py` and vendored `NearAICloudVerifier` |
| External dependency | dstack verifier at `DSTACK_VERIFIER_URL` |
| Enforced binding | `tls_spki_sha256` |
| Evidence endpoint | `https://cloud-api.near.ai/v1/attestation/report` |

## Verification algorithm

The provider bridge asks NEAR AI for a report with a fresh nonce and verifies the `gateway_attestation` component:

1. Require a gateway attestation and `tls_cert_fingerprint`.
2. Send the TDX quote, event log, and VM configuration to the configured dstack verifier and require `is_valid: true`.
3. When both `app_compose` and `compose_hash` are present, require `SHA256(UTF8(app_compose))` to equal the reported hash.
4. Parse the 64-byte `report_data` from the same quote bytes accepted by the dstack verifier.
5. Require a request nonce and signing address.
6. Verify the report-data layout:

   ```text
   report_data[0:32] = SHA256(signing_address || tls_cert_fingerprint)
   report_data[32:64] = request nonce
   ```

7. When an NVIDIA payload is present, require its nonce to match and require the NVIDIA verifier to succeed.
8. Emit one router-scoped TLS SPKI binding.

The bridge fails if the report-data value, nonce, signing address, or TLS fingerprint is absent. This closes the historical path where the verifier attempted to read `report_data` from a field the dstack verifier did not return.

## Channel binding and forwarding

The accepted TDX quote covers the signing address, nonce, and TLS SPKI digest through `report_data`. The gateway pins that digest against the live NEAR AI HTTPS certificate before sending the request.

The verifier cache omits the model from its key because every configured model uses the same router channel. The receipt records the selected model; the session records the shared router evidence.

## Session claims

| Claim | Mapping |
| --- | --- |
| `tee_attested` | Asserted from the verified TDX quote and bound TLS channel. |
| `tcb_up_to_date` | Asserted only for `UpToDate`; another surfaced status is refuted; absence is unknown. |
| `gpu_attested` | Unknown in the current session mapping because the bridge does not emit GPU verdict fields in `provider_claims`. |
| `os_known_good` | Unknown. |
| `serving_software_known_good` | Unknown. |
| `model_weights_provenance` | Unknown. |

## Limitations

- The bridge verifies the gateway component only. It does not fetch or verify `model_attestations[]` for the request's model.
- Nothing in the emitted session identifies the downstream model CVM that served a response.
- A non-`UpToDate` TCB status can remain `is_valid` and is represented as a refuted typed claim, not a bridge failure.
- Missing compose material is not a hard failure. A present compose/hash mismatch is rejected, but the current code does not require both values to exist.
- GPU verification can run when the gateway report includes a payload, but the resulting GPU fields are not preserved in the emitted provider claims.
- The bridge process defaults `DSTACK_VERIFIER_URL` to `http://localhost:8080`. The live suite overrides its default to `http://localhost:18080`.

## Tests and reproduction

Run the report-data tamper test:

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
