# Phala direct review

Date: 2026-11-23 PST (currency check). Original review 2026-06-10 UTC, updated
2026-06-11 for OS-image classification.

> [!NOTE]
> This is a dated admissions audit. It records the evidence and decision from
> the dates above. Source paths refer to the repository layout at the time. Use
> [verification.md](verification.md) for the adapter algorithm, claims, and
> limitations.
> The [November currency check](#currency-check-2026-11-23-pst) records the
> deployed producers and the current conditions for inclusion.

## Adapter update (2026-11-23 PST)

The adapter resolves the current production dstack OS images from its seed map
and supports optional pins for the measured compose. TCB status and OS
classification remain informational. See [Verification](verification.md) for
the pin format and claim mapping.

## Currency check (2026-11-23 PST)

The audit used verified session evidence from the production gateway. The
trust boundary remains the per-model dstack CVM.

- Phala Inference Guard (PIG) was the primary attestation producer. Published
  image matches identified `v0.12.41` (`d2b62a6a`).
  [PIG release](https://github.com/Phala-Network/phala-inference-guard/releases/tag/v0.12.41)
- The check also covered dstack-vllm-proxy `v0.2.20` (`4120cd26`). Its release
  commit is the merge commit for the version 2 attestation change.
  [vllm-proxy release](https://github.com/Dstack-TEE/vllm-proxy/releases/tag/v0.2.20)
  [Version 2 change](https://github.com/Dstack-TEE/vllm-proxy/pull/10)
- The observed production dstack OS versions were 0.6.5, 0.6.3, and 0.5.15.
  Upstream released 0.6.6 and 0.5.16 on November 20.
  [dstack 0.6.6](https://github.com/Dstack-TEE/dstack/releases/tag/v0.6.6)
  [dstack 0.5.16](https://github.com/Dstack-TEE/meta-dstack/releases/tag/v0.5.16)

### Verdict

Phala Direct is **acceptable with conditions** for version 2 TLS channel
binding. Strict confidential-inference admission is withheld until log
handling is shown to keep request content inside the TEE and the runtime
policy is reviewed. Strict inclusion also requires PIG source review,
reviewed Compose and image pins, enforced TCB and OS policy, and
model-weight provenance.

### Live check (20:08–20:20 PST)

- Attestation version 2 is live on all checked origins.
- All checked origins run production OS images.
- The verified sessions recorded TCB status `UpToDate` and TDX debug mode
  disabled.
- For all checked origins, the audit recomputed the stored evidence digest, the
  `report_data` binding of the signing address, TLS SPKI and request nonce,
  and the SHA-256 of `app_compose` against the measured `compose-hash` event.
  The live TLS SPKI matched the attested fingerprint in each case.
- `scripts/dstack_os_image.py` resolved the attested OS image hashes
  as verified production releases.
- The measured production compose places TLS termination inside the CVM.

### Open findings

1. **Producer review.** PIG's version 2 binding matches the adapter's
   requirements. The broader producer and ingress source review remains open.
   [PIG attestation source](https://github.com/Phala-Network/phala-inference-guard/blob/v0.12.41/internal/runtime/attestation/report.go)
2. **Log handling (criterion 5).** The measured production Compose exports
   container and kernel logs to a collector outside the CVM with
   credential-only redaction. PIG documents that it logs no request bodies.
   The serving-engine logs were not reviewed, so whether request content
   reaches the collector remains unverified.
   [PIG logging documentation](https://github.com/Phala-Network/phala-inference-guard/blob/v0.12.41/docs/OBSERVABILITY.md)
3. **Compose and image provenance (criteria 6, 7 and 13).** The adapter checks
   Compose integrity. Phala publishes no measurements ahead of rollout. A
   partial read of the measured Compose found settings that need review,
   including privileged serving containers. The full runtime policy was not
   reviewed. Some measured image digests remain unmapped to a published
   release. Strict inclusion still requires a reviewed allowlist and public
   pre-rollout publication of measurements.
4. **TCB and OS policy (criterion 14).** The adapter records both claims.
   Strict inclusion requires enforcing them.
5. **Model weights.** Weight provenance remains unknown to the gateway.

### Verification limits

- The audit checked the gateway's stored request nonces. A successful report
  fetch with an auditor-chosen nonce remains unverified.
- The production gateway's sessions establish live adapter compatibility
  for the checked origins. The deployed gateway revision and other origins
  remain unverified.
- Review of the measured production Compose's runtime policy and mapping
  every measured image digest to a published release remain open.
- Source-to-image reproducibility and whether request content can leave the
  TEE through logs remain unverified.

## Original review (2026-06-10 UTC)

Provider: `phala-direct` — direct connection to a Phala dstack-vllm-proxy attestation
endpoint, one per model. Expected to be superseded by an ACI-compatible server later
(`aci-service` is the eventual target shape); this is the pre-ACI "direct" path.

> Binding is **derive-and-bind**: the TLS SPKI is read from the version-2 attestation
> report, proven via the `report_data` binding inside the dstack-verified quote, and then
> pinned on the forward connection. There is no static TLS pin in config.

### Verdict

Acceptable as a TLS-SPKI-bound direct-model provider **once the proxy serves attestation
version 2** (custom-domain SPKI bound into `report_data`, TLS terminated inside the CVM by
a dstack-ingress sidecar). The channel binding and freshness are sound. Known-good
software/OS provenance pinning is the main open item before strict-release inclusion.

### Claim status (honest source map)

Tracking [audit-criteria.md](../audit-criteria.md). Each claim notes whether the gateway
can *prove* it on this path or only record an operator assertion.

| Claim | Status | Source |
| --- | --- | --- |
| `tee_attested` | **Proven** | dstack-verifier `is_valid` + report_data binding |
| `gpu_attested` | **Hardware-proven from the attested GPU shape** | The sound source is the CPU-TEE quote plus measured software that attests the GPU locally. dstack OS attests every attached GPU at boot and measures the outcome into RTMR3; the quote also binds the GPU count through `vm_config`. The adapter requires the boot attestation for every attested GPU and asserts `gpu_attested` from it, or refutes it for a CPU-only CVM (see [GPUs](verification.md#gpus)). The gateway-side NRAS result is auxiliary metadata (`gpu_verified` + `gpu_*`), and a runtime GPU failure never rejects. |
| `tcb_up_to_date` | **Surfaced** | dstack `details.tcb_status` (emitted as `tcb_status`); a strict freshness policy is still **TODO** — dstack-verifier may accept `OutOfDate` |
| `os_known_good` | **Classified (dev-vs-prod), bound to attestation; not gated** | dstack `os_image_hash` / MRTD / RTMRs reproduce a *real* dstack release. The image's `is_dev` flag is cryptographically bound to the attested `os_image_hash` (`os_image_hash = SHA256(sha256sum.txt)`, which pins `SHA256(metadata.json)`), so `resolve_os_image` decides `production_os_image` soundly (`os_image_is_dev` / `os_image_version` also surfaced). It is **recorded, not a gate**; a reviewed reproducible-build allowlist for the *vllm-proxy compose/image* is still a separate **TODO** |
| `serving_software_known_good` | **Integrity only** | `SHA256(app_compose) == compose_hash` proves compose integrity, but the compose/image digest is **not** checked against an allowlist on this path (**TODO** — operator-asserted today) |
| `model_weights_provenance` | **Not derived** | nothing in the attestation proves the loaded weights (operator-asserted only) |

### TODOs before strict-release inclusion

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

### Producer-side dependency

Requires the proxy change that adds attestation `version=2` (custom-domain SPKI binding),
implemented on the vllm-proxy branch `feat/versioned-attestation-tls-spki`
(`src/app/quote/tls_cert.py`, `_build_report_data`, the `/v1/attestation/report` `version`
param). Without it the gateway rejects the upstream (no `tls_cert_fingerprint`).
