# ACI service verification

Use the `aci-service` provider for an upstream that publishes a canonical ACI
report and runs on dstack with Intel TDX. The gateway verifies this path in
native Rust and enforces an attested TLS SPKI before forwarding.

## Summary

| Property | Value |
| --- | --- |
| Provider configuration | `"provider": "aci-service"` |
| Verifier | `AciServiceUpstreamVerifier` in `src/aci/verifier/aci_service.rs` |
| Verifier ID | `aci-service/v2` |
| Report | `GET /v1/aci/attestation?nonce=<fresh-64-hex>` |
| CPU evidence | Intel TDX quote verified with `dcap_qvl` |
| Workload measurement | dstack event-log replay to RTMR3, including `app-id` and `compose-hash` |
| Key custody | dstack KMS signature chain for the receipt key |
| Enforced channel | `tls_spki_sha256` |

The verifier does not use the Python provider bridge. The `pap` CLI runs the
same ACI §9.1 checks with its own implementation in
`apps/desktop/cli/aci/verifier/`.

## Required configuration

An `aci-service` upstream must provide:

- at least one `accepted_subjects` value or `accepted_image_digests` value;
- at least one `accepted_dstack_kms_root_public_keys` value; and
- an HTTPS `base_url` whose service publishes an attested TLS key.

The gateway refuses to load an `aci-service` entry without these policy
values.

Use a measured subject as the identity anchor:

```text
app-id:0x<hex-encoded-dstack-app-id>
```

The verifier derives that value from the RTMR3-verified event log. The upstream
keyset may omit `subject`; if it includes one, it must exactly match the
measured value.

See [Configuration reference](../../configuration-reference.md#upstream-fields)
for all fields, defaults, timeouts, and cache settings.

## Verification algorithm

For an uncached verification, the gateway generates a fresh 32-byte nonce,
fetches the canonical ACI report, and runs these checks in order. The first
failing check becomes the failure reason.

1. **Quote.** Requires the quote's 64-byte report-data slot to contain the ACI
   `report_data` value followed by zeros, fetches DCAP collateral from the
   configured PCCS, verifies the quote to the Intel root, and checks that the
   reported TEE type matches the quote.
2. **Binding.** Recomputes the workload keyset's JCS digest and the
   nonce-bound ACI statement, and requires them to match
   `workload_keyset_digest` and `report_data`.
3. **Expiry.** Requires the current time to be before the keyset's
   `not_after`.
4. **Provenance.** Requires source provenance and a published `app_compose`,
   replays the dstack runtime event log to the quote's RTMR3, requires
   `sha256(UTF8(app_compose))` to match the pre-`system-ready` `compose-hash`
   event, and extracts the measured dstack `app-id`.
5. **Custody.** Verifies the dstack KMS signature chain for the receipt key
   (see [Key custody](#key-custody)), then applies the identity policy: the
   measured app ID must be in `accepted_subjects`, or the report's
   `source_provenance.image_digest` must be in `accepted_image_digests`.
6. **Channel.** Selects the attested TLS SPKI that applies to the upstream
   origin.

These rejections identify common tampering:

| Change | Rejection |
| --- | --- |
| Edited keyset entry, such as a `tls_public_keys` SPKI | `WorkloadKeysetDigestMismatch` |
| Edited keyset with a recomputed digest, or a wrong nonce | `ReportDataMismatch` |
| Report data the quote does not carry | `QuoteReportDataMismatch` |
| Keyset past `not_after` | `KeysetExpired` |
| Subject or image digest outside the policy | `PolicyRejected` |

`tests/upstream_verifier.rs` covers the binding rejections.

## Key custody

The gateway derives its receipt and E2EE keys from dstack KMS. The custody
entry in the report's evidence proves part of that:

- The KMS root signs an app key for the measured app ID, and the app key signs
  the KMS public key for the `aci.receipt.ed25519.v1` purpose. The verifier
  recovers both signatures and requires the root to be in
  `accepted_dstack_kms_root_public_keys`.
- The entry's receipt public key must be one of the keyset's
  `receipt_signing_keys`.

The signature chain covers the KMS public key, not the published Ed25519
receipt key. That the receipt key comes from the KMS derivation is established
by reviewing the gateway source that the measured compose runs. `pap` runs the
same custody check under `--accept-subject app-id:0x<hex>` and
`--accept-dstack-kms-root-public-key`.

An attested TLS SPKI does not by itself show that the TLS private key is inside
the TEE. That is established only by reviewing the compose that runs the TLS
terminator.

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
opens. A binding mismatch invalidates the cached result, and the gateway
re-verifies and retries up to 2 times before the request fails.

A successful result stores the exact ACI report as session evidence. The claim
mapper records `tee_attested` as verifier-derived. It does not promote TCB
freshness, OS provenance, serving-software provenance, GPU attestation, or
model-weight provenance from this verifier into asserted typed claims.

## What `verified` means

A verified result establishes that:

- a genuine TDX quote binds the fresh ACI report and workload keyset;
- the dstack event log replays to the quote's RTMR3;
- the published compose preimage matches the measured `compose-hash`;
- the receipt key's custody entry chains to an accepted KMS root for the
  measured app ID;
- the configured identity policy accepted the report; and
- the actual upstream TLS connection is restricted to an SPKI from the
  attested keyset.

## Limitations

- `accepted_image_digests` compares an allowlisted value with the report's
  self-declared `source_provenance.image_digest`. The verifier does not bind
  that field to the measured compose. Use a measured `accepted_subjects` app ID.
- The verifier proves that the compose bytes were measured. It does not rebuild
  images, the launcher, dependencies, compiler output, or source from that
  compose.
- DCAP verification returns the collateral TCB status, but this path does not
  enforce an accepted-status policy or expose the status as a typed session
  claim.
- The custody check covers the receipt key. The E2EE custody entries are not
  matched against the keyset, and no custody entry covers TLS.
- The event-log and KMS checks do not independently reconstruct and accept a
  reviewed dstack OS image from MRTD and RTMR0-2.
- No GPU evidence or model-weight provenance is verified by this adapter.
- A service-wide keyset with several TLS SPKIs produces one stored session per
  binding even though the transport accepts any listed binding for the origin.

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
