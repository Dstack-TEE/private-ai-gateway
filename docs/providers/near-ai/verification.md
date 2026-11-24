# NEAR AI verification

The NEAR AI adapter verifies the public cloud gateway as one router-scoped TDX
channel. A verified session attests this gateway channel. Backend model
workload identity remains unverified.

| Property | Value |
| --- | --- |
| Attestation scope | Router |
| Verifier | `scripts/provider_verifier/nearai.py` and vendored `NearAICloudVerifier` |
| External dependency | dstack verifier at `DSTACK_VERIFIER_URL` (default `http://localhost:8080`) |
| Enforced binding | `tls_spki_sha256` |
| Evidence endpoint | `https://cloud-api.near.ai/v1/attestation/report` |

## Verification algorithm

The provider bridge asks NEAR AI for a report with a fresh nonce and
`include_tls_fingerprint=true`. It requires a non-empty `model_attestations[]`
with a non-empty string `intel_quote` in the first entry. Every entry must
report a `model_name` that exactly matches the configured upstream model ID
sent in the request. An alias that resolves to a different name fails this
check.

The report fetch and the gateway's dstack and NVIDIA verifier requests use a
3-second connect timeout and a 10-second read timeout. The Rust verifier
deadline bounds the subprocess across those calls.

`NearAICloudVerifier.verify_gateway_component` checks the
`gateway_attestation` component.

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

The verifier caches and refreshes verification by upstream model ID so each
model passes the name check. An explicit model name mismatch on refresh
immediately replaces even a current verified entry for that model because the
upstream now reports a different model. Requests reuse the mismatch for up to
60 seconds. If evidence is unavailable, the verifier keeps any still-valid
verified entry. Otherwise, it caches the failure for up to 10 seconds, capped
by the time between a scheduled refresh and the verified entry's expiry.
The receipt records the selected model, and the session records router
channel evidence.

Middleware injects a per-user `cache_salt`, honored by NEAR's TEE vLLM models. See [Prefix-cache isolation](../../prefix-cache-isolation.md).

Image generation for `black-forest-labs/FLUX.2-klein-4B` uses this same router
TLS attestation. It does not establish the image model instance, workload,
or weights provenance. NEAR image responses carry no token usage; middleware
billing uses `images.count` and synthesizes `usage.cost` from image pricing.

The round-1 live checks against `cloud-api.near.ai` rejected these tampered inputs:

| Tampered input | Rejection |
| --- | --- |
| Quote | `Dstack verification failed: Quote verification failed` |
| Request nonce | `Report data check failed: mismatch` |
| `tls_cert_fingerprint` | `Report data check failed: mismatch` |

## Session claims

| Claim | Mapping |
| --- | --- |
| `tee_attested` | Asserted as hardware-proven from the verified TDX quote and bound TLS channel. |
| `tcb_up_to_date` | Hardware-proven from the dstack verifier's TCB status: asserted for `UpToDate`, refuted for any other reported status, unknown when absent. |
| `gpu_attested` | Unknown. Supplemental GPU-check presence and acceptance are preserved in `claims.extra.gateway_gpu_check`, without an authenticated device-identity or CPU-binding claim. |
| `os_known_good` | Unknown. |
| `serving_software_known_good` | Unknown. |
| `model_weights_provenance` | Unknown. |

## Limitations

- No workload pin. Any dstack CVM that passes the dstack verifier with matching `report_data` is accepted as the NEAR AI gateway. The adapter does not compare the compose, app ID, or image with a reviewed value.
- The model name check uses response metadata. The TDX quote still covers the
  gateway channel only. The nested model quote is checked for presence.
- Configure canonical upstream IDs from `/v1/model/list`. NEAR resolves
  admin-controlled aliases on the server. See the
  [round-2 findings](review.md#open-findings).
- Nothing in the emitted session identifies the downstream model CVM that served a response.
- A non-`UpToDate` TCB status can remain `is_valid` and is represented as a refuted typed claim, not a bridge failure.
- Missing compose material is not a failure. A compose that does not match a present hash is rejected, but the adapter does not require both values.
- Supplemental GPU verification can run when the gateway report includes a payload. The bridge preserves its presence and acceptance in `gateway_gpu_check`, and reviewed material includes those outcomes. The NVIDIA path decodes JWTs without signature verification and does not bind device identity to the serving CPU, so it cannot establish authenticated GPU identity or continuity.

## Tests and reproduction

Run the hermetic report-data, model selection and bridge checks.

```sh
cargo test --locked --test soundness_report_data --test session_material_soundness
CARGO_BUILD_JOBS=4 cargo test --locked --lib near_ai_caches_model_failures_and_refreshes_healthy_siblings
```

For a live verifier run, start a trusted dstack verifier and set
`DSTACK_VERIFIER_URL` explicitly. The
[2026-11-23 PST live check](review.md#live-check-2026-11-23-20072012-pst) used
dstack-verifier 0.6.1 and the attestation endpoint answered without credentials.
The bridge sends a bearer token when one is configured.

```sh
export DSTACK_VERIFIER_URL='http://localhost:18080'
python3 - <<'PY' | uv run python scripts/private_ai_provider_verifier.py
import json

print(json.dumps({
    "api_version": "aci.provider-verifier.request.v1",
    "provider": "near-ai",
    "upstream_name": "near-ai-live",
    "url_origin": "https://cloud-api.near.ai",
    "model_id": "Qwen/Qwen3.8-27B",
    "forwarded_body_hash": "sha256:" + "0" * 64,
    "required": True,
    "timeout_seconds": 300,
}))
PY
```
