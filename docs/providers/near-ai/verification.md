# NEAR AI — attested session verification & binding

- **TEE:** Intel TDX (CPU) + NVIDIA Confidential Compute (GPU)
- **Session binding:** `tls_spki_sha256` (router); per-enclave sessions bound per response by `provider_tee` signatures
- **Verifier:** bridge (`verify_nearai`) → vendored `confidential_verifier`
  (`NearAICloudVerifier.verify_gateway_component` → `_verify_component`) + an external
  dstack-verifier service (`DSTACK_VERIFIER_URL`).
- **Status:** sound (the `report_data` binding was fixed in commit `ca7ddbd`).
- **Audit:** see [review.md](review.md).

## Per-enclave binding (model scope)

The bridge verifies NEAR's router as below **and** every model enclave NEAR
reports for the requested model (`/v1/attestation/report?…&provider=near`), with
the same component checks: TDX quote, event log and OS image (dstack verifier),
compose hash, `report_data` binding of the enclave's signing key, TLS fingerprint
and nonce, and NVIDIA GPU evidence (NRAS, nonce-matched). The result is
model-scoped (`attested_scope: "model"`) and lists the verified enclave signers.

The backend seals the router's shared session plus one session per verified
enclave signer. After a buffered completion, `NearAiBackend` fetches
`GET /v1/signature/{chat_id}?signing_algo=ecdsa` and requires:

- `signature_kind` is `provider_tee` (the serving enclave signed, not the router);
- the signed text is exactly `<model>:<sha256(request.forwarded bytes)>:<sha256(response.received bytes)>`;
- the EIP-191 signer recovers to the reported `signing_address`.

On success the receipt cites that enclave's session, whose claims carry the
enclave's GPU, TCB and OS verdicts (the router's TCB and OS verdicts fold in,
since the router relays the traffic). Every outcome is recorded in an
`upstream.response_attested` receipt event with the signature, so a verifier can
re-check it offline. A signer outside the verified set, a `gateway` signature,
or a streamed completion cites the router session.

Requests to NEAR carry `x-no-aliasing: true` and `accept-encoding: identity`,
which NEAR requires for enclave-signed responses.

**Operational note.** The dstack verifier resolves OS images from
`download.dstack.org/os-images/mr_<hash>.tar.gz`. Some NEAR model enclaves run
`dstack-nvidia-0.5.11`, which that server does not carry (404). The image is
published on the `Dstack-TEE/meta-dstack` v0.5.11 release; its
`sha256(sha256sum.txt)` equals the reported `os_image_hash`
(`a6eafc5f…7b02`). Preload it into the verifier's image cache
(`images/<os_image_hash>/`: `bzImage`, `initramfs.cpio.gz`, `ovmf.fd`,
`metadata.json`, `sha256sum.txt`) after checking that hash, or the enclave fails
the OS check and only the router is verified.

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

- Only the **gateway** component is verified here, and NEAR AI is treated as a
  router (`AttestationScope::PerRouter`): its nested per-model TD quotes are
  **not** fetched or checked. The gateway does not re-verify them and nothing
  binds them to the instance that served a given request, so the attested session
  is the gateway *channel* only. A request-bound, per-instance model attestation
  is a roadmap item, recorded on the receipt rather than in the session.
- Requires a reachable dstack-verifier at `DSTACK_VERIFIER_URL` (default `:18080`).
- NVIDIA NRAS JWT-signature hardening is a tracked defense-in-depth follow-up.

## Reproduce

```bash
cd <private-ai-gateway checkout>
set -a; . .env; set +a
export DSTACK_VERIFIER_URL="http://localhost:18080"
echo '{"api_version":"aci.provider-verifier.request.v1","provider":"near-ai",
  "upstream_name":"near-ai-live","url_origin":"https://cloud-api.near.ai",
  "model_id":"google/gemma-4-31B-it",
  "forwarded_body_hash":"sha256:'"$(printf '0%.0s' {1..64})"'","required":true,
  "timeout_seconds":300}' \
  | uv run python scripts/private_ai_provider_verifier.py
```
