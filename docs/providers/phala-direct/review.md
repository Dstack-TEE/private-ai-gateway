# PhalaDirect Review

Provider: `phala-direct` — direct connection to a Phala dstack-vllm-proxy attestation
endpoint, one per model. Expected to be superseded by an ACI-compatible server later
(`aci-service` is the eventual target shape); this is the pre-ACI "direct" path.

> **How the gateway verifies this provider:** see [verification.md](verification.md).
> Binding is **derive-and-bind**: the TLS SPKI is read from the version-2 attestation
> report, proven via the `report_data` binding inside the dstack-verified quote, and then
> pinned on the forward connection. There is no static TLS pin in config.

## Verdict

Acceptable as a TLS-SPKI-bound direct-model provider **once the proxy serves attestation
version 2** (custom-domain SPKI bound into `report_data`, TLS terminated inside the CVM by
a dstack-ingress sidecar). The channel binding and freshness are sound. Known-good
software/OS provenance pinning is the main open item before strict-release inclusion.

## Claim status (honest source map)

Tracking [audit-criteria.md](../audit-criteria.md). Each claim notes whether the gateway
can *prove* it on this path or only record an operator assertion.

| Claim | Status | Source |
| --- | --- | --- |
| `tee_attested` | **Proven** | dstack-verifier `is_valid` + report_data binding |
| `gpu_attested` | **Derived from CPU + code (TODO), not NRAS** | The sound source is the **CPU-TEE quote + measured serving software**: the serving software, attested inside the CPU-TEE quote, locally attests the GPU and establishes the encrypted CPU↔GPU CC channel, refusing to serve otherwise. So `gpu_attested` should be derived from `tee_attested` ∧ `serving_software_known_good` — and is therefore **Unknown** today until serving-software provenance is pinned (see that row). The gateway-side `NvidiaGpuVerifier` / NRAS result is recorded as **auxiliary metadata only** (`gpu_verified` + `gpu_*`); it is an existence oracle, never the source of truth, and a GPU failure never rejects. |
| `tcb_up_to_date` | **Surfaced** | dstack `details.tcb_status` (emitted as `tcb_status`); a strict freshness policy is still **TODO** — dstack-verifier may accept `OutOfDate` |
| `os_known_good` | **Classified (dev-vs-prod), bound to attestation; not gated** | dstack `os_image_hash` / MRTD / RTMRs reproduce a *real* dstack release. The image's `is_dev` flag is cryptographically bound to the attested `os_image_hash` (`os_image_hash = SHA256(sha256sum.txt)`, which pins `SHA256(metadata.json)`), so `resolve_os_image` decides `production_os_image` soundly (`os_image_is_dev` / `os_image_version` also surfaced). It is **recorded, not a gate**; a reviewed reproducible-build allowlist for the *vllm-proxy compose/image* is still a separate **TODO** |
| `serving_software_known_good` | **Integrity only** | `SHA256(app_compose) == compose_hash` proves compose integrity, but the compose/image digest is **not** checked against an allowlist on this path (**TODO** — operator-asserted today) |
| `model_weights_provenance` | **Not derived** | nothing in the attestation proves the loaded weights (operator-asserted only) |

## TODOs before strict-release inclusion

- Pin the accepted vllm-proxy image/compose digest (allowlist) on the PhalaDirect path —
  today the config's `accepted_image_digests` / `accepted_subjects` fields are wired to
  the native `aci-service` path, not this external bridge.
- `production_os_image` is now decided from dstack's published image metadata (bound to the
  attested `os_image_hash`), so dev images are flagged today (the deployed fleet is all
  `is_dev: true`). Remaining: decide whether to **gate** on it (reject non-prod) for a
  strict release tier, vs. the current record-only behavior.
- Set and enforce a TCB freshness policy (reject `OutOfDate` per criterion 14).
- Confirm the production TLS endpoint is terminated by the attested CVM (dstack-ingress
  sidecar), with no off-TEE terminator — otherwise the SPKI binding is vacuous.
- Live end-to-end review against a real PhalaDirect endpoint + dstack-verifier (this doc
  is from the implementation, not yet a live audit).

## Producer-side dependency

Requires the proxy change that adds attestation `version=2` (custom-domain SPKI binding),
implemented on the vllm-proxy branch `feat/versioned-attestation-tls-spki`
(`src/app/quote/tls_cert.py`, `_build_report_data`, the `/v1/attestation/report` `version`
param). Without it the gateway rejects the upstream (no `tls_cert_fingerprint`).
