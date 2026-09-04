# ACI service verification

Use the `aci-service` provider for an upstream that publishes a canonical ACI
report and runs on dstack with Intel TDX. The gateway verifies this path in
native Rust and enforces an attested TLS SPKI before forwarding.

This page is the living reference for operators configuring the verifier and
reviewers deciding what its `verified` result means.

## Current contract

| Property | Value |
| --- | --- |
| Provider configuration | `"provider": "aci-service"` |
| Verifier | `AciServiceUpstreamVerifier` in `src/aci/verifier/aci_service.rs` |
| Verifier ID | `aci-service/v2` |
| Report | `GET /v1/aci/attestation?nonce=<fresh-64-hex>` |
| CPU evidence | Intel TDX quote verified with `dcap_qvl` |
| Workload measurement | dstack event-log replay to RTMR3, including `app-id` and `compose-hash` |
| Key custody | dstack KMS chain for the receipt-signing key |
| Enforced channel | `tls_spki_sha256` |

The verifier does not use the Python provider bridge. It shares its ACI §9.1
appraisal code with the `aci` CLI.

## Required configuration

An `aci-service` upstream must provide:

- at least one `accepted_subjects` value or `accepted_image_digests` value;
- at least one `accepted_dstack_kms_root_public_keys` value; and
- an HTTPS `base_url` whose service publishes an attested TLS key.

The strongest current identity anchor is a measured subject in this form:

```text
app-id:0x<hex-encoded-dstack-app-id>
```

The verifier derives that value from the RTMR3-verified event log. The upstream
keyset may omit `subject`; if it includes one, it must exactly match the
measured value.

See [Configuration reference](../../configuration-reference.md#upstream-fields)
for all fields, defaults, timeouts, and cache settings.

## Verification algorithm

For an uncached verification, the gateway:

1. Generates a fresh 32-byte nonce and fetches the canonical ACI report.
2. Recomputes the workload keyset's JCS digest and the nonce-bound ACI
   statement. It rejects a mismatched `workload_keyset_digest`, mismatched
   `report_data`, or expired `not_after`.
3. Parses the TDX quote, fetches DCAP collateral from the configured PCCS,
   verifies the quote, checks the reported TEE type, and requires the quote's
   64-byte report-data slot to contain the ACI `report_data` value followed by
   zeros.
4. Requires source provenance and a published `app_compose`.
5. Replays the dstack runtime event log and requires the resulting RTMR3 to
   match the verified quote.
6. Requires `sha256(UTF8(app_compose))` to match the pre-`system-ready`
   `compose-hash` event, then extracts the measured dstack `app-id`.
7. Applies the configured identity policy to that measured app ID or the
   accepted image-digest path described under limitations.
8. Verifies the dstack KMS signature chain for the receipt-signing key against
   an accepted KMS root and the measured app ID.
9. Selects an attested TLS SPKI that applies to the upstream origin.

Any missing required evidence, policy mismatch, quote failure, expired keyset,
custody failure, or unusable channel binding returns a failed verification
event.

## How the TLS binding is selected

The workload keyset is part of the nonce-bound report, so changing a
`tls_public_keys` entry changes the keyset digest and breaks the report-data
binding.

For a keyset with no domain-scoped entries, every listed service-wide SPKI is
returned as an allowed binding for the configured origin.

For a keyset with any domain-scoped entry, the evidence must also publish:

```json
{
  "downstream_tls_binding": {
    "domain": "worker.example.com",
    "spki_sha256": "<64-hex>"
  }
}
```

The verifier normalizes the domain, requires it to match the configured origin
host, and requires the selected SPKI to be one of the keyset entries applicable
to that host. Only that selected value becomes the session's
`tls_spki_sha256` binding.

The upstream transport then pins the peer certificate's SubjectPublicKeyInfo
digest to the verified binding before any prompt bytes are forwarded. A report
that verifies without an enforceable TLS binding is rejected.

## Cache and session behavior

Only successful results are cached. A cached result expires at the earlier of:

- `verified_at + verifier_cache_seconds`; or
- the workload keyset's `not_after`.

Every forward still enforces the cached TLS binding against the connection it
opens. A binding mismatch invalidates the cache and allows one fresh
verification before the candidate fails.

A successful result stores the exact ACI report as session evidence. The
current generic claim mapper records `tee_attested` as verifier-derived. It
does not promote TCB freshness, OS provenance, serving-software provenance,
GPU attestation, or model-weight provenance from this verifier into asserted
typed claims.

## What `verified` means

A verified result establishes that:

- a genuine TDX quote binds the fresh ACI report and workload keyset;
- the dstack event log replays to the quote's RTMR3;
- the published compose preimage matches the measured `compose-hash`;
- the configured identity policy accepted the report;
- the receipt key passed the configured dstack KMS custody check; and
- the actual upstream TLS connection is restricted to an SPKI from the
  attested keyset.

It does not establish every possible workload claim. Apply the limitations
below when deciding whether to accept this path.

## Limitations

- `accepted_image_digests` currently compares an allowlisted value with the
  report's self-declared `source_provenance.image_digest`. The verifier does
  not bind that field to the measured compose. Prefer a measured
  `accepted_subjects` app ID until that conformance gap is closed.
- The verifier proves that the compose bytes were measured. It does not rebuild
  images, the launcher, dependencies, compiler output, or source from that
  compose.
- DCAP verification returns the collateral TCB status, but this path does not
  currently enforce an accepted-status policy or expose the status as a typed
  session claim.
- The dstack custody check covers the receipt-signing role. It does not yet
  establish custody for every E2EE and TLS private key in the workload keyset.
- The event-log and KMS checks do not independently reconstruct and accept a
  reviewed dstack OS image from MRTD and RTMR0-2.
- No GPU evidence or model-weight provenance is verified by this adapter.
- A service-wide keyset with several TLS SPKIs produces one stored session per
  binding even though the transport accepts any listed binding for the origin.
  This is a known session-model gap for multi-key channels.

These gaps are also tracked in
[Reference implementation conformance gaps](../../reviews/aci-spec-conformance-gaps.md).

## Tests and reproduction

Run the native verifier tests:

```bash
cargo test --locked --test upstream_verifier
cargo test --locked aci::verifier
```

The local and Phala smoke suites exercise the provider through real upstream
configuration:

```bash
bash scripts/local_multi_upstream_smoke.sh
bash scripts/phala_multi_upstream_smoke.sh
```

The smoke suites have additional environment and infrastructure prerequisites.
Read the [live test guide](../../live-e2e-test-suite.md) before running them.
