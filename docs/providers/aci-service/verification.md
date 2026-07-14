# AciService (first-party) — attested session verification & binding

- **TEE:** Intel TDX (CPU) + NVIDIA Confidential Compute, on dstack
- **Session binding:** `tls_spki_sha256`
- **Verifier:** native Rust — `AciServiceUpstreamVerifier`
  (`src/aci/verifier/aci_service.rs`). No bridge / Python; this is the path
  for the gateway's own ACI-compatible workers.
- **Status:** sound (covered by `tests/upstream_verifier.rs`).
- **Audit:** none — first-party path; [`audit-criteria.md`](../audit-criteria.md) targets
  third-party providers.

## What is verified

`AciServiceUpstreamVerifier` fetches `GET /v1/aci/attestation?nonce=<random>`
(the spec §5 report) from the worker with a fresh 16-byte nonce and verifies
it natively:

1. **ACI report binding** (`validate_aci_report_binding`,
   `src/aci/verifier/report.rs` — the spec §10.1(2–3) chain):
   base64-decode `workload_keyset_b64`; the SHA-256 of the decoded bytes must
   equal the reported `workload_keyset_digest`; rebuild the §4.2 statement
   `{"keyset_digest":…,"nonce":…,"purpose":"aci.report_data.v1"}` for the
   supplied nonce and check its SHA-256 equals `report_data`; check the keyset
   is not expired (`now < not_after`). Freshness comes from the nonce; the
   cached verification never outlives `not_after`.
2. **Identity policy** — the attested keyset `subject` must be in
   `accepted_subjects`, or the report's provenance `image_digest` in
   `accepted_image_digests` (§4 identity anchors); otherwise
   `PolicyRejected`.
3. **DCAP quote** — `dcap_qvl` verifies the TDX quote against fetched
   collateral, the claimed `tee_type` matches the verified quote, and the
   quote's report-data slot binds the validated `report_data` zero-padded to
   64 bytes.
4. **dstack event log + app-id** — replay the RTMR event log and extract the
   dstack app-id.
5. **dstack KMS key custody** — verify the KMS signature chain for the
   published keys against an accepted root in
   `accepted_dstack_kms_root_public_keys` (§4.3 custody; the chain covers the
   released key's k256 counterpart, and the link to the published Ed25519 key
   rests on the measured workload code).

## What binds the session

The TLS SPKIs are attested through
`workload_keyset.tls_public_keys[].spki_sha256_hex`. The keyset digest is the
SHA-256 of the exact served keyset bytes, which include `tls_public_keys` —
and that digest is folded into `report_data` and therefore into the verified
quote. Tampering any TLS entry changes the keyset bytes, so it trips
`WorkloadKeysetDigestMismatch` (recomputed digest ≠ reported) and
`ReportDataMismatch` (the statement digest no longer matches the quote's
report data).

For a domain-scoped keyset, the verifier also requires
`attestation.evidence.downstream_tls_binding` to name the requested origin
host and a SPKI present in the attested keyset. Only that selected SPKI
becomes the enforced `tls_spki_sha256` channel binding. Service-wide keysets
without per-domain entries keep the previous behavior: every service-wide TLS
SPKI is accepted for the origin.

## What a tamper rejects

Wrong nonce → `ReportDataMismatch`; tampered keyset or digest →
`WorkloadKeysetDigestMismatch` / `ReportDataMismatch`; expired keyset →
`KeysetExpired`; unaccepted subject/image → `PolicyRejected`; quote that does
not bind the report data → `QuoteReportDataMismatch`. Unit-tested in
`tests/upstream_verifier.rs`.

## Transport enforcement

The backend enforces the verified `tls_spki_sha256` against the upstream HTTPS
connection before forwarding.

## Notes

- This is the path the gateway uses for its own GPU workers once they expose an
  ACI-compatible `/v1/aci/attestation`. It is kept minimal today; see the roadmap's
  "Provider Soundness and Strict Pins" and the deferred standalone-Phala work.
- Policy inputs (`accepted_subjects`, `accepted_image_digests`,
  `accepted_dstack_kms_root_public_keys`, `pccs_url`) are configured per
  upstream, not via broad process-level env.

## Source & platform provenance, and TCB status

Tracking criteria 13–14 of [audit-criteria.md](../audit-criteria.md) (AciService has no
separate `review.md`):

- **Software provenance** (worker code → reviewed source): via the
  `accepted_subjects` / `accepted_image_digests` policy plus `app_compose`.
  **TODO:** populate and pin the reviewed allowlist (kept minimal today).
- **Platform/OS provenance** (dstack guest OS / firmware → reviewed reproducible build):
  the dstack event-log RTMR replay and KMS-root custody are verified, but the reviewed
  dstack OS image digest is **TODO** to pin.
- **TCB status / freshness**: **TODO** — `verify_dcap_quote` does not check
  `verified.status`; add an `UpToDate` / allowlist check per criterion 14.

## Reproduce

Driven through upstream entries with `provider: "aci-service"` against workers
that expose `/v1/aci/attestation`; see
`scripts/phala_multi_upstream_smoke.sh` and
`scripts/local_multi_upstream_smoke.sh`.
