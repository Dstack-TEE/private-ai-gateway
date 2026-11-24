# Tinfoil verification

The Tinfoil adapter uses Tinfoil's official Python SDK to verify its confidential model router and pins the router's attested TLS public key.

| Property | Value |
| --- | --- |
| Attestation scope | Router |
| Verifier | `scripts/provider_verifier/tinfoil.py` using `tinfoil.SecureClient` |
| Verifier ID | `private-ai-verifier/tinfoil/v1` |
| Enforced binding | `tls_spki_sha256` |
| Source repository | `tinfoilsh/confidential-model-router`, overridable with `provider_options.tinfoil_repo` |

## Verification algorithm

The bridge constructs `SecureClient(enclave=<origin host>, repo=<repository>)`, calls `verify()`, and reads the verification document. The SDK owns the hardware and provenance verification chain:

1. Resolve the repository's latest release tag and artifact digest through
   Tinfoil's GitHub proxy.
2. Verify the Sigstore bundle, Rekor transparency-log inclusion, and a GitHub
   Actions certificate identity for the repository and exact release tag
   (`refs/tags/<tag>`). This yields the release measurement.
3. Fetch the hardware report from `<host>/.well-known/tinfoil-attestation` and verify it. For SEV-SNP, the SDK verifies the report signature with the VCEK, the VCEK to ASK to ARK certificate chain to AMD's root, and the guest policy, which requires `Debug=false`, `MigrateMA=false`, and a minimum TCB. For TDX, it verifies DCAP collateral and rejects debug TDs.
4. Extract the TLS public-key fingerprint from `report_data[0:32]`.
5. Compare the enclave measurement with the Sigstore-proven release measurement.

The bridge then requires:

- `security_verified` to be true.
- A non-empty TLS public-key fingerprint.
- Router scope, either selected explicitly by the SDK or implied by the router repository.
- A verified release digest that matches a configured release pin when any are set.

The emitted evidence preserves the repository, release tag and digest, code and
enclave fingerprints, TLS fingerprint, HPKE key, overall verdict, and per-step
statuses. The session exposes the verified release tag and digest in `claims.extra`.

## Release pins

To admit only reviewed releases, add one or more entries of the form
`tinfoil-release:sha256:<64 lowercase hex>` to the upstream's
`accepted_subjects`. Use the `release_digest` from the reviewed release's
verified evidence. With no Tinfoil release entries, the SDK's repository
verification is sufficient. A missing or unlisted digest fails with
`provenance_not_accepted` when pins are set. If the provider publishes a release
at rollout, verification can fail until an operator reviews and adds its digest.

## Channel binding and forwarding

The hardware signature covers the whole report, including the TLS fingerprint in `report_data`. The bridge emits that verified fingerprint as `tls_spki_sha256`.

The gateway compares the binding with the live HTTPS certificate before forwarding.
All models behind the same Tinfoil router share the verifier cache and session.
The served model remains a receipt field. When the admitted router's final
response includes a `Tinfoil-Enclave` value of 1 to 256 bytes, the gateway
records it in the signed `upstream.tinfoil_enclave` receipt event. This is the
router's report about the selected replica, with no independent gateway
attestation of that replica.

## Session reuse

TDX and SNP verification returns an SDK summary that omits the authenticated
quote and security-relevant platform attributes. The adapter declares that this
evidence does not bind platform state. The gateway gives each fresh verifier run
a local round token and retains it on cache hits, separating session fingerprints
across independent rounds even when summaries match. The token is not added to
provider evidence, sessions or receipts and does not expose hidden platform changes.
Evidence-independent reuse requires the SDK to expose the authenticated quote
used by `SecureClient.verify()` so a complete projection can be reviewed.

See [session migration](../../attested-session-system.md#migration-pre-456-to-this-version)
for existing pins and rollback.
## Session claims

| Claim | Mapping |
| --- | --- |
| `tee_attested` | Asserted as hardware-proven from the official hardware verifier and bound TLS channel. |
| `tcb_up_to_date` | Asserted as verifier-derived. The TCB gate is the SDK's static minimum. The SDK exposes no separate vendor TCB status. |
| `serving_software_known_good` | Asserted as verifier-derived from the Sigstore-proven release measurement. |
| `gpu_attested` | Unknown. |
| `os_known_good` | Unknown. The verifier does not classify the OS image. |
| `model_weights_provenance` | Unknown. |

## Tamper results

In the 2026-06 soundness pass, each of these tampered reports from
`inference.tinfoil.sh` (decompress the SEV report, flip a byte, recompress,
verify) failed with `Attestation signature verification failed`, because the
AMD signature covers the whole report:

- changed `report_data`;
- changed measurement; and
- changed signature.

## Limitations

- Middleware sends a per-user, provider-separated `user_cache_secret` on text-generation requests. Router `v0.0.179` sets `CACHE_SALT_ENABLED: "true"` in its measured `tinfoil-config.yml`. The gateway accepts later signed releases, which could change that policy. The router uses the secret and the authenticated caller identity to derive its own salt. Caller `cache_salt` strings only subdivide the user's partition, and the gateway strips raw caller cache fields. See the [live isolation observation](../../prefix-cache-isolation.md#observed-enforcement) and [disposable-CVM runbook](../../cache-salt-cvm-test.md). Hermetic coverage: `cargo test --test middleware_completion cache_salt`.
- The gateway delegates the hardware, TCB, and source-provenance checks to
  Tinfoil SDK `0.15.0`, pinned in `pyproject.toml` and `uv.lock`. An upgrade
  changes the verifier trust root and requires review. The SDK verifies the
  v2 attestation document, which carries no nonce. In this SDK the TDX
  `MR_SEAM` allowlist includes modules 1.5.16, 1.5.25, 2.0.08, and 2.0.14.
- The default path proves the confidential router channel. Per-model TEE coverage depends on the verified router's own model-enclave policy and is not independently recorded by this gateway.
- Verification needs egress to the router's `/.well-known/tinfoil-attestation` endpoint, `kds-proxy.tinfoil.sh` for AMD VCEK certificates, Tinfoil's GitHub attestation proxy, and Sigstore trust material.
- The adapter does not establish GPU-to-router binding or model-weight provenance in the ACI session claims.

## Reproduce

From the repository root:

```sh
request_hash="sha256:$(printf '0%.0s' {1..64})"
jq -n --arg hash "$request_hash" '{
  api_version: "aci.provider-verifier.request.v1",
  provider: "tinfoil",
  upstream_name: "tinfoil-live",
  url_origin: "https://inference.tinfoil.sh",
  model_id: "gpt-oss-120b",
  forwarded_body_hash: $hash,
  required: true,
  timeout_seconds: 300
}' | uv run python scripts/private_ai_provider_verifier.py
```

See the dated [router admissions review](review.md) for the inspected provider revision and its admission conditions.
