# NEAR AI — attested session verification & binding

- **TEE:** Intel TDX (CPU) + NVIDIA Confidential Compute (GPU)
- **Session binding:** `tls_spki_sha256`
- **Verifier:** bridge (`verify_nearai`) → vendored `confidential_verifier`
  (`NearAICloudVerifier.verify_gateway_component` → `_verify_component`) + an external
  dstack-verifier service (`DSTACK_VERIFIER_URL`).
- **Status:** sound (the `report_data` binding was fixed in commit `ca7ddbd`).
- **Audit:** see [review.md](review.md).

## What is verified

`verify_nearai` fetches a report with a fresh nonce
(`NearaiProvider(include_tls_fingerprint=True).fetch_report` from
`cloud-api.near.ai/v1/attestation/report`), then `_verify_component("gateway", …)` does:

1. **Verify the TDX quote.** The dstack-verifier service verifies the quote, event log,
   and VM config (RTMR replay). `is_valid` must be true.
2. **Verify the compose hash.** `SHA256(app_compose)` must equal the reported
   `compose_hash`.
3. **Verify the report_data binding.** Parse `report_data` from the *verified* quote
   bytes (`_tdx_report_data_hex`, TDX v4 offset `quote[48+520 : 48+584]`) and run
   `verify_report_data(report_data, signing_address, request_nonce, tls_cert_fingerprint)`.
   For the TLS-fingerprint format that means
   `report_data[0:32] == SHA256(signing_address ‖ tls_cert_fingerprint)` and
   `report_data[32:64] == nonce`. **Fail closed** if `report_data`, the nonce, or the
   signing address is unavailable.
4. **Verify the GPU.** Check the GPU evidence nonce equals the request nonce, then
   `NvidiaGpuVerifier` POSTs to NVIDIA NRAS over TLS and requires a passing result.

## What binds the session

The TLS public-key fingerprint, the signing address, and the request nonce are all
folded into `report_data`, which lives inside the DCAP/dstack-verified quote. So the
`tls_spki_sha256` the gateway enforces is proven to belong to the attested TDX
workload — not merely copied from the report JSON.

> Why this matters: before the fix, `_verify_component` read `report_data` from the
> dstack-verifier's result (a field it never returns), so the whole binding check was
> silently skipped. A wrong nonce or a swapped `tls_cert_fingerprint` still "verified",
> which meant no freshness and an unauthenticated TLS-SPKI binding.

## What a tamper rejects

Confirmed live against `cloud-api.near.ai`:

- Tampered quote → `Dstack verification failed: Quote verification failed`.
- Wrong nonce → `Report data check failed: mismatch`.
- Swapped `tls_cert_fingerprint` (MITM attempt) → `Report data check failed: mismatch`.

The hermetic regression test `tests/soundness_report_data.rs` pins the binding logic.

## Transport enforcement

The backend enforces the verified `tls_spki_sha256` against the upstream HTTPS
connection before forwarding.

## Notes

- Only the **gateway** component is verified here; NEAR AI's nested model attestations
  are recorded as gateway-owned evidence (`model_attestations_sha256`) but not
  re-verified on the lease path.
- Requires a reachable dstack-verifier at `DSTACK_VERIFIER_URL` (default `:18080`).
- NVIDIA NRAS JWT-signature hardening is a tracked defense-in-depth follow-up.

## Reproduce

```bash
set -a; . /home/h4x/workspace/redpill/.env; set +a
export DSTACK_VERIFIER_URL="http://localhost:18080"
cd /home/h4x/workspace/redpill/private-ai-gateway
echo '{"api_version":"aci.provider-verifier.request.v1","provider":"near-ai",
  "upstream_name":"near-ai-live","url_origin":"https://cloud-api.near.ai",
  "model_id":"google/gemma-4-31B-it",
  "forwarded_body_hash":"sha256:'"$(printf '0%.0s' {1..64})"'","required":true,
  "timeout_seconds":300}' \
  | uv run python scripts/private_ai_provider_verifier.py
```
