# ArmetAI Per-Model TDX Review

Date: 2026-09-29 UTC.

> [!NOTE]
> This is the provider's **self-assessment** against the
> [audit criteria](../audit-criteria.md), submitted with the adapter. It is not
> an independent audit. Items marked **Provider evidence required** are
> commitments that a reviewer must confirm against source and a live endpoint
> before a decision. Use [verification.md](verification.md) for the adapter
> algorithm, claims and limitations.

Source repos reviewed: **TODO** (guest image build, attestation service, and
in-TD TLS proxy repositories with commits, to be listed before review).

## Verdict

**Decision pending.** The adapter verifies the live `poc1.armet.ai` endpoint
(see [Live probe](#live-probe-2026-10-02)), but source provenance, TLS key
custody and the privacy boundary have not been reviewed.

The proposed trust model is:

1. The trust boundary is one **direct model instance**: a single Intel TDX TD
   on ArmetAI-operated KVM/QEMU hosts, serving one model at one HTTPS origin,
   with NVIDIA confidential-computing GPUs passed through to it.
2. The TD's quote commits, through `report_data`, to a fresh gateway nonce, the
   TLS SPKI it serves, the exact NVIDIA GPU evidence it collected, and the
   model ID.
3. The gateway pins the verified TD measurements and launch configuration
   against operator-configured `accepted_subjects`.
4. The GPU evidence must pass NRAS with a signed, nonce-matched result for
   every GPU.
5. The gateway pins the attested TLS SPKI on every forwarded request.

The expected decision, once the provider evidence below is supplied, is
**acceptable with conditions**. The conditions would be the required
measurement pins and the TODOs under criteria 7 and 13.

## Criteria

### 1. Workload identity

- **Met by the adapter:** DCAP-verified TDX quote, a fresh 32-byte nonce per
  verification, and debug TDs rejected. The identity is the pinned measurement
  subject over MRTD, RTMR0-3, TD_ATTRIBUTES, XFAM, MRCONFIGID, MROWNER and
  MROWNERCONFIG.
- **Provider evidence required:** the TLS private key is generated inside the
  TD at boot, held only in TD memory, and never exported. The certificate is
  obtained from inside the TD, for example with ACME.

### 2. Channel binding

- **Met by the adapter:** `tls_spki_sha256`, committed in `report_data`, is
  enforced on every forwarded request by the SPKI-pinned client. A certificate
  rotation fails closed until the gateway re-verifies.
- **Provider evidence required:** TLS terminates inside the TD. No load
  balancer, CDN or ingress terminates TLS outside it.
- **Network path:** `poc1.armet.ai` resolves to an AWS address
  (`52.53.179.221`) running HAProxy in TCP passthrough mode, which forwards to
  the TD without terminating TLS. The live probe saw the TD-attested SPKI on
  the public origin. HAProxy is outside the trust boundary: SPKI pinning means
  it cannot read or alter traffic without the TD's private key, only drop or
  delay it. It sees connection metadata (client addresses, timing, sizes) but
  no content, and ArmetAI states it does not log that metadata.

### 3. Gateway soundness

Not applicable. The trust boundary is a direct model instance, not a router.
The adapter declares `model` scope and the gateway refuses a `router` scope
claim.

### 4. Model selection

- **Met by the adapter:** the gateway sends the canonical provider model ID.
  The report must echo it and `report_data` must commit to it, so an alias
  cannot select another TD's model.
- **Limitation:** the weights actually loaded are a property of the measured
  software; `model_weights_provenance` is Unknown.

### 5. Privacy boundary

**Provider evidence required.** Show the following, with source:

- No prompt, completion or tool payload logging, persistence, metrics or
  traces leave the TD.
- Billing events carry only token counts.
- No debug endpoints exist in production images.

### 6. Runtime policy

- **Met by the adapter:** debug TDs are rejected, and TD attributes and XFAM
  are pinned.
- **Launch configuration (`MRCONFIGID`):** the host sets `MRCONFIGID` to the
  hash of the TD's `initdata.toml`, the launch-time configuration it hands to
  the TD. `MRCONFIGID` is part of the pin, so any change to `initdata.toml`
  changes the subject and fails closed until re-pinned.
- **Provider evidence required:**
  - the TD recomputes the hash of the `initdata.toml` it actually consumes and
    refuses to start unless it equals its own `MRCONFIGID`. Otherwise the host
    could hand the TD different initdata from the one the pin covers;
  - the published `initdata.toml` for each release, with the hash algorithm,
    so reviewers can recompute `MRCONFIGID` and judge its security-relevant
    settings;
  - an immutable, dm-verity-protected root filesystem measured through the
    kernel command line;
  - no SSH or remote shell;
  - credentials loaded only inside the TD;
  - no off-TEE TLS termination.

### 7. Release and measurement updates

**TODO. This blocks strict inclusion.** ArmetAI must publish, before each
production rollout:

- the release commit and image digest;
- the raw MRTD, RTMR0-3 and launch configuration fields, and the
  `initdata.toml` behind `MRCONFIGID`;
- the resulting `tdx-measurement` subject for each VM shape;
- a description of security-relevant changes.

It must also publish an emergency-rollout policy. The adapter fails closed on
any unpublished measurement.

### 8. Lease lifecycle

Met by the gateway's standard per-model lease and non-destructive refresh. No
provider-specific session material is used.

### 9. Request fidelity

Chat and streaming pass end to end through the gateway (see
[Live probe](#live-probe-2026-10-02)). **Provider evidence required:** tools,
structured outputs and error paths, as advertised.

### 10. Load balancing and cache

Per-instance endpoints: one TD per origin. There is no provider-side router in
the trust path. Cache affinity is not claimed.

### 11. Receipts

Met by the adapter. Evidence is the exact report bytes, with a `sha256:`
digest, recorded in the attested session.

### 12. Negative checks

Covered hermetically by `tests/provider_verifier/armet_ai_soundness.py`. It
checks replayed nonces, a swapped TLS key, another model, swapped GPU evidence,
each pinned field, debug, revoked TCB and the NRAS failure modes.

Live: a request for a model the TD does not serve (`GLM-5.3-Other`) was
refused by the endpoint with `409`, and the verifier failed closed. The
certificate-rotation and VM-shape checks are **TODO**.

### 13. Source and platform provenance

**TODO. This blocks strict inclusion.**

- **Software provenance:** today the pin is operator trust in a measurement.
  A reproducible guest image build is required so reviewers can recompute the
  pinned values from source.
- **Platform provenance:** the TDX module `MR_SEAM` and `TEE_TCB_SVN` are
  recorded in provider claims but not pinned. Firmware (TDVF) provenance must
  be documented.

### 14. Platform TCB freshness

- **Met by the adapter:** `Revoked` is rejected. Other statuses are recorded,
  with advisory IDs, and surface as the hardware-proven `tcb_up_to_date`
  claim, the same bar as the other TDX adapters.
- **Provider commitment:** ArmetAI operates its own hosts and commits to
  keeping the TCB `UpToDate`, so relying parties can require it through claims
  policy.

## Live probe (2026-10-02)

The bridge verifier (`scripts/private_ai_provider_verifier.py`) was run
directly against `https://poc1.armet.ai` for provider model `GLM-5.3-Flash`,
then end to end through a locally run gateway (dstack simulator for the
gateway's own keys).

| Check | Result |
| --- | --- |
| Wire contract (v2 fields, nonce echo, `nvidia_payload` string) | Pass |
| Reported SPKI equals live certificate SPKI | Pass (`e3e15952…1cd5`) |
| DCAP quote verification with collateral | Pass; TCB `UpToDate`, no advisories |
| Debug TD | Off (`td_attributes` `0000001000000000`) |
| `report_data` commitment | Pass |
| Placeholder pin | Failed only at the pin step, as expected |
| Pinned run | `verified`, model scope, `tls_spki_sha256` binding |
| NRAS GPU gate | Pass: 8 × GH100, every GPU nonce-matched |
| Wrong model | `409` from the endpoint; verifier failed closed |

Observed values:

| Field | Value |
| --- | --- |
| Subject | `tdx-measurement:sha256:0265ed947a1c20c3ac84aae84c1a0088c215c46a390ae26575916cde58e788f6` |
| MRTD | `c78e2b8b…60b12a` |
| RTMR3 | all zeros (not extended) |
| MRCONFIGID | `a8b1759a…3c158b` (hash of `initdata.toml`) |
| MROWNER, MROWNERCONFIG | all zeros |
| MR_SEAM | `ab62561a…127f6b` |
| TEE_TCB_SVN | `0f010400000000000000000000000000` |

End to end through the gateway:

| Check | Result |
| --- | --- |
| Live E2E suite, quick profile | Pass: provider verification, SPKI-pinned forward, signed receipt, attested session |
| Receipt audit (`pap audit`, offline) | Receipt and `upstream-1` checks pass. `id-4` (the gateway's own source provenance) fails only because the gateway ran outside a dstack CVM. |
| Chat with `provider.aci_verified: true` | `200` with `X-Receipt-Id` |
| Streaming with `provider.aci_verified: true` | `200` with `X-Receipt-Id`, terminated by `[DONE]` |
| Pinned to a non-current `aci_session_ids` value | `412`, refused before forwarding |
| Session claims | `tee_attested`, `tcb_up_to_date`, `serving_software_known_good`, `gpu_attested` asserted; `os_known_good`, `model_weights_provenance` unknown |

Tools, structured outputs and error-path fidelity (criterion 9) are not yet
covered.

The subject was pinned for this test only. It becomes an acceptance decision
once the release behind it is published and reviewed (criteria 7 and 13).

## Hard reject conditions

| Condition | Status |
| --- | --- |
| No enforceable channel binding | Clear: `tls_spki_sha256` enforced. |
| Plaintext leaves the boundary | Pending provider evidence (criteria 2 and 5). |
| Unverified model path | Clear: per-model TD, model committed in `report_data`. |
| Catalog metadata as proof | Clear: not used. |
| Aliases select an unrelated model | Clear: exact model ID match and commitment. |
| Debug bypass without pin change | Clear: debug rejected, TD attributes pinned. |
| Unpublished measurement swaps | Adapter fails closed; publication process is TODO (criterion 7). |
| Verifier fails open | Clear: every step gates, including GPU. |
| No model-scoped evidence | Clear. |
| Semantic post-processing | Chat and streaming pass unmodified; tools and structured outputs pending (criterion 9). |

## Open questions for maintainers

1. Should the pin also cover `MR_SEAM`, or is TCB status sufficient for TDX
   module updates?
2. Would maintainers prefer a provider-neutral name for this adapter shape
   (a non-dstack, measurement-pinned TDX endpoint with a mandatory GPU gate),
   so other providers can reuse it?
3. Is gating on `UpToDate` for this provider preferred over the cross-provider
   `Revoked`-only bar?
