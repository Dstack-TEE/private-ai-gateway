# PhalaDirect — attested session verification & binding

- **TEE:** Intel TDX (CPU) + NVIDIA Confidential Compute (GPU), Phala dstack.
- **Topology:** the gateway connects **directly** to each model's own
  dstack-vllm-proxy endpoint (one HTTPS endpoint per model, configured as its own
  upstream entry). No hosted `api.redpill.ai` / `cloud-api.phala.network` hop.
- **Session binding:** `tls_spki_sha256` (custom-domain leaf SPKI).
- **Verifier:** bridge (`verify_phala_direct`) → vendored `confidential_verifier`
  (`DstackVerifier`, `verify_report_data`, `NvidiaGpuVerifier`) + an external
  dstack-verifier service (`DSTACK_VERIFIER_URL`).
- **Requires:** the proxy serving attestation **version 2** (see
  [Producer requirement](#producer-requirement)).
- **Status:** sound for the TLS-SPKI binding; known-good software/OS provenance is a
  TODO (see [review.md](review.md)).

## Producer requirement

The binding only works if the proxy binds its custom-domain TLS SPKI into the quote.
The proxy reads its dstack-ingress certificate (`TLS_CERT_PATH`), computes
`SHA256(SubjectPublicKeyInfo DER)` of the leaf, and serves attestation **version 2**:

```
GET /v1/attestation/report?version=2&signing_algo=ecdsa&nonce=<hex>
report_data[0:32]  = SHA256(signing_address ‖ tls_cert_fingerprint)
report_data[32:64] = nonce
response.tls_cert_fingerprint = <SHA256(SPKI DER) hex>
```

Soundness depends on TLS terminating **inside** the CVM (the dstack-ingress sidecar's
certbot key is TEE-resident). A gateway-managed / off-TEE TLS terminator would make the
SPKI binding vacuous.

## What is verified

`verify_phala_direct` GETs `{base_url}/v1/attestation/report?version=2&signing_algo=ecdsa`
with a fresh nonce (and the configured `bearer_token`), then:

1. **Require a TLS fingerprint.** `tls_cert_fingerprint` must be present — an older proxy
   that ignored `version=2` cannot be TLS-pinned, so it is rejected.
2. **Reject debug-mode TDs.** The TDX quote's `TD_ATTRIBUTES` TUD byte (offset 168) must
   be zero — a debug-mode TD exposes its CPU state and private memory to the host, so the
   TEE guarantee does not hold. (Shared `tdx_debug_enabled`, applied to every TDX provider.)
3. **Verify the TDX quote.** The dstack-verifier service verifies the quote, event log,
   and VM config (RTMR replay). `is_valid` must be true.
4. **Verify the compose hash.** `SHA256(app_compose)` must equal the reported
   `compose_hash`.
5. **Verify the report_data binding.** Parse `report_data` from the *verified* quote
   bytes (`_tdx_report_data_hex`, TDX v4 offset `quote[48+520 : 48+584]`) and run
   `verify_report_data(report_data, signing_address, nonce, tls_cert_fingerprint)`:
   `report_data[0:32] == SHA256(signing_address ‖ tls_cert_fingerprint)` and
   `report_data[32:64] == nonce`. **Fail closed** if `report_data`, the nonce, or the
   signing address is unavailable.
6. **Record the GPU evidence — supplemental, not a gate.** Check the GPU evidence nonce
   against the request nonce and run `NvidiaGpuVerifier` (NRAS), but **do not fail** on a
   GPU error. A standalone gateway-side NRAS check only proves a CC-capable GPU *exists*
   for a nonce; it does not prove that GPU is bound to this CPU TEE or serving this request.
   That binding is the measured serving software's job, attested *inside* the CPU-TEE quote
   — so GPU trust is subsumed by the CPU-TEE quote + serving-software provenance (steps
   2–4), and the NRAS result is recorded as `gpu_verified` / `gpu_evidence_*` metadata.

## What binds the session

The TLS SPKI fingerprint, the signing address (secp256k1 response-signing key), and the
request nonce are all folded into `report_data`, which lives inside the dstack-verified
quote. So the `tls_spki_sha256` the gateway enforces is proven to belong to the attested
TDX workload — not merely copied from the report JSON. The signing address is surfaced in
`provider_claims` so it can be pinned alongside the SPKI.

## What a tamper rejects

Pinned hermetically by `tests/phala_direct_bridge.rs` (→ `scripts/soundness_phala_direct.py`):

- Missing `tls_cert_fingerprint` (version-2 not served) → rejected.
- Swapped `tls_cert_fingerprint` (MITM attempt) → `report_data binding failed`.
- Wrong nonce → `report_data binding failed`.
- dstack quote invalid → rejected (the CPU-TEE gate).

GPU is **supplemental**, so a GPU evidence nonce mismatch or a failed NRAS result does
**not** reject — the upstream still verifies, and the outcome is recorded as
`gpu_verified: false` / `gpu_evidence_nonce_matched: false`. The shared report_data binding
logic is also pinned by `tests/soundness_report_data.rs`.

## Transport enforcement

The backend enforces the verified `tls_spki_sha256` against the upstream HTTPS connection
(`OpenAICompatibleBackend` → `SpkiPinVerifier`, which pins `SHA256(SPKI DER)`) before
forwarding any request.

## Provider claims recorded

`trust_boundary` (`phala-dstack-cvm`), `evidence_scope` (`model_instance`),
`canonical_model_id`, `attestation_version` (2), `tls_spki_from_report_data`,
`signing_address`, `report_data_nonce_matched`, `compose_hash_verified`, `tdx_debug_mode`
(always `false` — debug TDs are rejected), `tcb_status` (the granular dstack TCB status,
e.g. `UpToDate`), the OS-image provenance trio `os_image_hash` / `os_image_version` /
`os_image_is_dev` and the resolved `production_os_image` decision, and the supplemental GPU
metadata `gpu_verified`, `gpu_evidence_present`, `gpu_evidence_nonce_matched`, `gpu_arch`.

> **`production_os_image` is decided, not a TODO.** It is `false` for a dev image, `true`
> for a production image, and `null` only when the hash cannot be resolved (unknown image
> while offline). It is recorded metadata, **not a gate** — the deployed fleet currently
> runs dev images (`dstack-nvidia-dev-*`, `is_dev: true`), so gating here would reject them;
> the session layer decides policy. See [How the OS image is classified](#how-the-os-image-is-classified).

## How the OS image is classified

The decision is bound to the attestation, so the image download server cannot lie about it:

1. The dstack verifier returns `app_info.os_image_hash` and only reports `is_valid` when
   `os_image_hash_verified` (it reproduced MRTD/RTMRs from that exact image).
2. dstack derives `os_image_hash = SHA256(sha256sum.txt)`, and that manifest pins
   `SHA256(metadata.json)`. So `metadata.json`'s `is_dev` flag is **cryptographically bound**
   to the attested hash — flipping it changes `metadata.json` → `sha256sum.txt` →
   `os_image_hash`, which would no longer match the quote.
3. `resolve_os_image` (`scripts/dstack_os_image.py`) re-downloads
   `https://download.dstack.org/os-images/mr_{os_image_hash}.tar.gz`, **re-verifies both
   equalities**, then reads `is_dev`. `production_os_image = not is_dev`.

Known fleet images are seeded in `KNOWN_OS_IMAGES`, so the common path is offline; an
unseeded hash is resolved once and cached on disk. Re-verify or add an image with
`uv run python scripts/dstack_os_image.py <os_image_hash>`.

## Notes

- Requires a reachable dstack-verifier at `DSTACK_VERIFIER_URL` (default
  `http://localhost:8080`).
- The guest OS image is now **classified** (dev-vs-prod, bound to the attested
  `os_image_hash`) and surfaced as `production_os_image` — but it is recorded, **not gated**.
  Known-good pinning of the vllm-proxy image/compose digest is still **not** enforced
  (compose-hash *integrity* is proven, but the compose is not checked against an allowlist).
  See [review.md](review.md).
- GPU attestation is **supplemental metadata, not a security boundary** on this path. A
  gateway-side NRAS check is an online existence oracle (a CC-capable GPU exists for a
  nonce); it does not prove the GPU is bound to this CPU TEE. The sound model is that the
  CPU TEE's *measured serving software* (inside the quote) attests the GPU and sets up the
  encrypted CPU↔GPU CC channel, refusing to serve otherwise — so GPU trust is subsumed by
  the CPU-TEE quote + serving-software provenance. Verifying the NRAS JWT against NRAS' JWKS
  would not change this, so it is not treated as a gating requirement.

## Configuration

Each model is its own dstack-vllm-proxy endpoint, so today it is configured as **one
upstream entry per model** (its own `base_url`); see `deploy/upstreams.example.json`. The
verifier routes by `base_url` origin and derives the binding + every claim dynamically — no
TLS pin or claim is set in config. This collapses into a single `phala-direct` provider
entry whose `models` map carries a per-model `endpoint` once the rich multi-model-per-
provider config loader lands (that refactor is owned by the attested-session work and keeps
this verifier's per-`{model, endpoint}` wiring unchanged).

## Reproduce

```bash
set -a; . /home/h4x/workspace/redpill/.env; set +a
export DSTACK_VERIFIER_URL="http://localhost:8080"
cd /home/h4x/workspace/redpill/private-ai-gateway
echo '{"api_version":"aci.provider-verifier.request.v1","provider":"phala-direct",
  "upstream_name":"phala-direct-live","url_origin":"https://<model-endpoint>",
  "model_id":"<canonical-model>",
  "provider_options":{"phala_direct_bearer_token":"<api-key>"},
  "forwarded_body_hash":"sha256:'"$(printf '0%.0s' {1..64})"'","required":true,
  "timeout_seconds":300}' \
  | uv run python scripts/private_ai_provider_verifier.py
```
