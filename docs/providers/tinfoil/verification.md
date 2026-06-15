# Tinfoil — attested session verification & binding

- **TEE:** AMD SEV-SNP (TDX also supported) + NVIDIA Confidential Compute
- **Session binding:** `tls_spki_sha256`
- **Verifier:** the official `tinfoil` Python SDK
  (`SecureClient(enclave, repo).verify()`), called from the bridge
  (`scripts/private_ai_provider_verifier.py` → `verify_tinfoil`).
- **Status:** sound (replaced a hand-rolled, unsound verifier in commit `747b117`).
- **Audit:** see [review.md](review.md).

## What is verified

`verify_tinfoil` constructs `SecureClient(enclave=<host>, repo=<repo>)` (default repo
`tinfoilsh/confidential-model-router`), calls `.verify()`, and reads
`get_verification_document()`. The SDK runs Tinfoil's full reference chain — the same
checks as `tinfoilsh/verifier` — exposed as four steps:

1. **`fetch_digest`** — fetch the repo's latest release artifact digest.
2. **`verify_code`** — fetch the Sigstore bundle and verify it cryptographically:
   SCTs, **Rekor** transparency-log inclusion, and a **certificate identity** of
   `https://token.actions.githubusercontent.com` with the repo's tag-workflow pattern.
   This yields the golden code measurement (provenance bound to the open-source repo).
3. **`verify_enclave`** — verify the hardware report. For SEV-SNP: the report signature
   against the **VCEK**, the **VCEK → ASK → ARK** certificate chain to AMD's embedded
   root, and policy (`Debug=false`, `MigrateMA=false`, `SMT`, minimum TCB). For TDX:
   DCAP collateral + policy. Extracts `report_data[0:32]` as the TLS public-key
   fingerprint.
4. **`compare_measurements`** — the enclave's measurement must equal the
   Sigstore-attested code measurement.

`doc.security_verified` must be true.

## What binds the session

`report_data[0:32]` is the TLS public-key fingerprint, and the AMD signature covers the
whole report (including `report_data`). The bridge emits
`tls_spki_sha256 = doc.tls_public_key`, which equals `report_data[0:32]` — the exact
value the gateway already enforced, now cryptographically proven rather than read from
an unauthenticated report.

> Why this matters: the previous hand-rolled `_verify_snp` performed **no** AMD
> signature verification — it only compared the measurement to a *public* Sigstore
> value. A forged report with the public measurement and any `report_data` (any TLS
> key) passed. See [review.md](review.md) for the provider audit.

## What a tamper rejects

Confirmed live against `inference.tinfoil.sh` (decompress the SEV report, flip a byte,
recompress, verify):

- Tampered `report_data` → `Attestation signature verification failed`.
- Tampered measurement → `Attestation signature verification failed`.
- Tampered signature → `Attestation signature verification failed`.

(The signature covers the whole report, so every one of these is caught — unlike the
old verifier, which accepted `report_data` and signature tampering.)

## Transport enforcement

The backend enforces the verified `tls_spki_sha256` against the upstream HTTPS
connection before forwarding.

## Notes

- Verification needs egress to Tinfoil's endpoints: the attestation endpoint
  (`<host>/.well-known/tinfoil-attestation`), `kds-proxy.tinfoil.sh` (AMD VCEK),
  the GitHub attestation proxy, and Sigstore's TUF root.
- Router mode: by default this verifies the **router** enclave
  (`tinfoilsh/confidential-model-router`). Tinfoil is a router
  (`AttestationScope::PerRouter`): the attested session is that one verified
  enclave channel, shared by every model behind it, so verification is keyed on
  the channel and the served model is a receipt-level identifier. Per-model TEE
  coverage is delegated to the verified router, which attests the model enclaves
  it fronts. Override the repo via `provider_options.tinfoil_repo`.
- Pin the dependency deliberately; the SDK is the source of truth for the check set,
  so upgrades should be reviewed.

## Reproduce

```bash
cd /home/h4x/workspace/redpill/private-ai-gateway
echo '{"api_version":"aci.provider-verifier.request.v1","provider":"tinfoil",
  "upstream_name":"tinfoil-live","url_origin":"https://inference.tinfoil.sh",
  "model_id":"kimi-k2-6",
  "forwarded_body_hash":"sha256:'"$(printf '0%.0s' {1..64})"'","required":true,
  "timeout_seconds":300}' \
  | uv run python scripts/private_ai_provider_verifier.py
```
