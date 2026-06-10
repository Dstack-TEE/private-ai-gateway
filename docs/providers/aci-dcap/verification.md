# AciDcap (first-party Phala) — attested session verification & binding

- **TEE:** Intel TDX (CPU) + NVIDIA Confidential Compute, on dstack
- **Session binding:** `tls_spki_sha256`
- **Verifier:** native Rust — `AciDcapUpstreamVerifier` (`src/aci/verifier.rs`). No
  bridge / Python; this is the path for the gateway's own ACI-compatible workers.
- **Status:** sound (designed with the keyset-digest binding from the start;
  covered by `tests/upstream_verifier.rs`).
- **Audit:** none — first-party path; [`audit-criteria.md`](../audit-criteria.md) targets
  third-party providers.

## What is verified

`AciDcapUpstreamVerifier` fetches `GET /v1/attestation/report?nonce=<random>` from the
worker and verifies it natively:

1. **ACI report binding** (`validate_aci_report_binding`, `src/aci/verifier.rs`):
   - `workload_id` recomputed from the workload identity must equal the reported
     `workload_id`.
   - `workload_keyset_digest` recomputed from the keyset must equal the reported digest.
   - `report_data == SHA256(JCS(attestation_statement))`, where the statement is
     `{ workload_id, workload_keyset_digest, nonce }` — this places the keyset digest
     and the nonce into the quote's report_data.
   - The keyset endorsement signature (identity key over the keyset digest) verifies.
   - Freshness: `fetched_at <= now < stale_after`.
2. **DCAP quote** — `dcap_qvl` verifies the TDX quote against fetched collateral.
3. **dstack event log + app-id** — replay the RTMR event log and extract/accept the
   dstack app-id.
4. **dstack KMS identity custody** — verify the secp256k1 KMS signature chain against
   the accepted KMS root key.

## What binds the session

The TLS SPKIs come from `workload_keyset.tls_public_keys[].spki_sha256_hex`. The
`WorkloadKeyset.to_canonical_value()` (`src/aci/types.rs`) **includes** `tls_public_keys`,
so they are covered by `workload_keyset_digest` — which is, in turn, (a) checked against
the reported digest, (b) folded into `report_data` (and thus into the verified quote),
and (c) signed by the keyset endorsement. The TLS-SPKI binding is therefore triple-bound
to the attested workload.

## What a tamper rejects

Tampering any `tls_public_keys` entry changes `workload_keyset_digest`, which trips
three independent checks at once:

- `WorkloadKeysetDigestMismatch` (recomputed ≠ reported),
- `ReportDataMismatch` (statement digest no longer matches the quote's report_data),
- `KeysetEndorsementInvalid` (endorsement signature no longer verifies).

Other rejections: wrong nonce → `ReportDataMismatch`; bad endorsement →
`KeysetEndorsementInvalid`; stale report → `StaleReport`. Unit-tested:
`tests/upstream_verifier.rs::aci_report_binding_validation_{rejects_wrong_nonce,
rejects_bad_keyset_endorsement,accepts_self_consistent_report}`.

## Transport enforcement

The backend enforces the verified `tls_spki_sha256` against the upstream HTTPS
connection before forwarding.

## Notes

- This is the path the gateway uses for its own GPU workers once they expose an
  ACI-compatible `/v1/attestation/report`. It is kept minimal today; see the roadmap's
  "Provider Soundness and Strict Pins" and the deferred standalone-Phala work.
- Policy inputs (accepted workload ids / image digests / KMS root keys, PCCS URL) are
  configured per upstream, not via broad process-level env.

## Source & platform provenance, and TCB status

Tracking criteria 13–14 of [audit-criteria.md](../audit-criteria.md) (AciDcap has no
separate `review.md`):

- **Software provenance** (worker code → reviewed source): via the
  `accepted_workload_ids` / `accepted_image_digests` policy plus `app_compose`.
  **TODO:** populate and pin the reviewed allowlist (kept minimal today).
- **Platform/OS provenance** (dstack guest OS / firmware → reviewed reproducible build):
  the dstack event-log RTMR replay and KMS-root custody are verified, but the reviewed
  dstack OS image digest is **TODO** to pin.
- **TCB status / freshness**: **TODO** — `verify_dcap_quote` does not check
  `verified.status`; add an `UpToDate` / allowlist check per criterion 14.

## Reproduce

Driven through the gateway's `aci-dcap` upstream verifier mode against a worker that
exposes `/v1/attestation/report`; see `scripts/phala_multi_upstream_smoke.sh` and
`scripts/local_multi_upstream_smoke.sh`
(`PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER: aci-dcap`).
