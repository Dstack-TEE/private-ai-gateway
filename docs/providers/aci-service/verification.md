# AciService (first-party) — attested session verification & binding

- **TEE:** Intel TDX (CPU) + NVIDIA Confidential Compute, on dstack
- **Session binding:** `tls_spki_sha256`
- **Verifier:** native Rust — `AciServiceUpstreamVerifier`
  (`src/aci/verifier/aci_service.rs`). No bridge / Python; this is the path
  for the gateway's own ACI-compatible workers.
- **Versions:** ACI report wire format `aci/1`; verifier implementation
  `aci-service/v2`.
- **Status:** sound (designed with the keyset-digest binding from the start;
  covered by `tests/upstream_verifier.rs`).
- **Audit:** none — first-party path; [`audit-criteria.md`](../audit-criteria.md) targets
  third-party providers.

## What is verified

`AciServiceUpstreamVerifier` fetches `GET /v1/aci/attestation?nonce=<random>`
(the spec §4 report) from the worker and verifies it natively:

1. **ACI report binding** (`validate_aci_report_binding`,
   `src/aci/verifier/report.rs` — the spec §9.1(2–3) chain):
   the SHA-256 of the served `workload_keyset` object's JCS form must
   equal the reported `workload_keyset_digest`; rebuild the §3.2 statement
   `{"keyset_digest":…,"nonce":…,"purpose":"aci.report_data.v1"}` for the
   supplied nonce and check its SHA-256 equals `report_data`; check the keyset
   is not expired (`now < not_after`). Freshness comes from the nonce; the
   cached verification never outlives `not_after`.
2. **Identity policy** — the attested keyset `subject` must be in
   `accepted_subjects`, or the report's provenance `image_digest` in
   `accepted_image_digests` (§3 identity anchors); otherwise
   `PolicyRejected`.
3. **DCAP quote** — `dcap_qvl` verifies the TDX quote against fetched
   collateral.
4. **dstack event log, app-id, and Compose** — verify RTMR3, extract and
   accept the measured app-id, and verify the Compose preimage as described
   below.
5. **dstack KMS key custody** — verify the KMS signature chain for the
   published keys against an accepted root in
   `accepted_dstack_kms_root_public_keys` (§3.3 custody; the chain covers the
   released key's k256 counterpart, and the link to the published Ed25519 key
   rests on the measured workload code).

## How the measured Compose is verified

The ACI-service verifier connects `attestation.evidence.app_compose` to the
verified TDX quote as follows:

1. Verify the TDX quote and its nonce-bound `report_data`.
2. Recompute the dstack runtime-event digests, replay the boot event log, and
   require the result to equal RTMR3 in the verified quote.
3. Read the pre-`system-ready` `compose-hash` event and require
   `SHA256(UTF8(app_compose))` to equal that measured value.

These checks prove integrity and measurement binding. They do not prove that an
image, launcher, source revision, compiler, dependency, or OS build is
acceptable. Those trust-policy checks remain separate.

## What binds the session

The TLS SPKIs are attested through
`workload_keyset.tls_public_keys[].spki_sha256_hex`. The keyset's JCS form
**includes** `tls_public_keys`, so they are covered by
`workload_keyset_digest` — which is, in turn, (a) checked against the
reported digest and (b) folded into `report_data` (and thus into the
verified quote). The TLS-SPKI binding is therefore double-bound to the
attested workload.

For a domain-scoped keyset, the verifier also requires
`attestation.evidence.downstream_tls_binding` to name the requested origin host and a
SPKI present in the attested keyset. Only that selected SPKI becomes the enforced
`tls_spki_sha256` channel binding. Service-wide keysets without per-domain entries keep
the previous behavior: every service-wide TLS SPKI is accepted for the origin.

## What a tamper rejects

Tampering any `tls_public_keys` entry changes `workload_keyset_digest`, which
trips two independent checks at once:

- `WorkloadKeysetDigestMismatch` (recomputed ≠ reported),
- `ReportDataMismatch` (statement digest no longer matches the quote's
  report_data).

Other rejections: wrong nonce → `ReportDataMismatch`; expired keyset →
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
- Policy inputs (accepted keyset subjects / image digests / KMS root keys,
  PCCS URL) are configured per upstream, not via broad process-level env.

## Source & platform provenance, and TCB status

Tracking criteria 13–14 of [audit-criteria.md](../audit-criteria.md) (AciService has no
separate `review.md`):

- **Software provenance** (worker code → reviewed source): via the RTMR3-bound
  `app_compose`, followed by launcher/image and source-provenance policy. The
  native verifier proves the Compose preimage is measured. **TODO:** parse and
  enforce the reviewed launcher/image/source allowlist rather than accepting a
  measured app-id alone.
- **Platform/OS provenance** (dstack guest OS / firmware → reviewed reproducible build):
  the dstack event-log RTMR replay and KMS-root custody are verified, but the reviewed
  dstack OS image digest is **TODO** to pin.
- **TCB status / freshness**: **TODO** — `verify_quote_to_root` reports
  `status` but nothing gates on it. Add an `UpToDate` / allowlist check per
  criterion 14.

## Reproduce

Driven through upstream entries with `provider: "aci-service"` against workers
that expose `/v1/aci/attestation`; see
`scripts/phala_multi_upstream_smoke.sh` and
`scripts/local_multi_upstream_smoke.sh`.
