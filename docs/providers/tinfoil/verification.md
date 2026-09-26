# Tinfoil Verification

The Tinfoil adapter uses Tinfoil's official Python SDK to verify its confidential model router and pins the router's attested TLS public key.

| Property | Value |
| --- | --- |
| Attestation scope | Router |
| Verifier | `scripts/provider_verifier/tinfoil.py` using `tinfoil.SecureClient` |
| Verifier ID | `tinfoil-verifier/v1` |
| Enforced binding | `tls_spki_sha256` |
| Source repository | `tinfoilsh/confidential-model-router`, overridable with `provider_options.tinfoil_repo` |

## Verification algorithm

The bridge constructs `SecureClient(enclave=<origin host>, repo=<repository>)`, calls `verify()`, and reads the verification document. The SDK owns the hardware and provenance verification chain:

1. Resolve the repository's latest release artifact digest.
2. Verify the Sigstore bundle, Rekor transparency-log inclusion, and a GitHub Actions certificate identity for the repository's release workflow. This yields the release measurement.
3. Fetch the hardware report from `<host>/.well-known/tinfoil-attestation` and verify it. For SEV-SNP, the SDK verifies the report signature with the VCEK, the VCEK to ASK to ARK certificate chain to AMD's root, and the guest policy, which requires `Debug=false`, `MigrateMA=false`, and a minimum TCB. For TDX, it verifies DCAP collateral and rejects debug TDs.
4. Extract the TLS public-key fingerprint from `report_data[0:32]`.
5. Compare the enclave measurement with the Sigstore-proven release measurement.

The bridge then requires:

- `security_verified` to be true;
- a non-empty TLS public-key fingerprint; and
- router scope, either selected explicitly by the SDK or implied by the router repository.

The emitted evidence preserves the repository, release digest, code and enclave fingerprints, TLS fingerprint, HPKE key, overall verdict, and per-step statuses.

## Channel binding and forwarding

The hardware signature covers the whole report, including the TLS fingerprint in `report_data`. The bridge emits that verified fingerprint as `tls_spki_sha256`.

The gateway compares the binding with the live HTTPS certificate before forwarding. All models behind the same Tinfoil router share the verifier cache and session. The served model remains a receipt field.

## Session claims

| Claim | Mapping |
| --- | --- |
| `tee_attested` | Asserted from the official hardware verifier and bound TLS channel. |
| `tcb_up_to_date` | Asserted as verifier-derived because Tinfoil's overall verifier gates TCB policy but does not expose a separable raw status. |
| `serving_software_known_good` | Asserted as verifier-derived from the Sigstore-proven release measurement. |
| `gpu_attested` | Unknown. |
| `os_known_good` | Unknown. |
| `model_weights_provenance` | Unknown. |

## Limitations

- The gateway delegates the hardware, TCB, and source-provenance check set to the Tinfoil SDK version pinned in `pyproject.toml` and `uv.lock`. An upgrade changes the verifier trust root and must be reviewed.
- The default path proves the confidential router channel. Per-model TEE coverage depends on the verified router's own model-enclave policy and is not independently recorded by this gateway.
- `release_digest` is preserved as evidence but is not checked against a separate operator allowlist in gateway configuration.
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
  model_id: "kimi-k2-6",
  forwarded_body_hash: $hash,
  required: true,
  timeout_seconds: 300
}' | uv run python scripts/private_ai_provider_verifier.py
```

See the dated [router admissions review](review.md) for the inspected provider revision and its admission conditions.
