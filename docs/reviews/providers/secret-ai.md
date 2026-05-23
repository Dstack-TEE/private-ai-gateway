# SecretAI Direct-Instance Review

Date: 2026-05-22 UTC.
Provider endpoint: `https://secretai-jedi.scrtlabs.com` (attestation on `:29343`,
Ollama-via-Caddy inference on `:21434`).
Trust boundary: single verified SecretVM running AMD SEV-SNP with NVIDIA H100
("HOPPER") GPU, fronted by `secret-ai-caddy`.

Source repos reviewed:

- `scrtlabs/secretvm` at `ed4115cfe266365ffa0e34a0c4effadb6066040f`
- `scrtlabs/secretvm-verify` at `e80a54ffe8aae17e4103e6b62ed7f51f3eeb1801`
- `scrtlabs/secret-ai-caddy` at `fa469aa19a315bb6546fe465b0519a66cc4a45db`
- `scrtlabs/secret-vm-ops` at `3bbb653c6b7569149ca5b8e16084383ad91d3ecf`
- `scrtlabs/secretvm-cli` at `5356c121a5456bb1a45fe9fc107358c22ccb3bd7`

Supporting documentation:

- `docs.scrt.network/secret-network-documentation/secretvm-confidential-virtual-machines/attestation/obtaining-attestation-data`

## Verdict

SecretAI is **acceptable with conditions** as a direct-instance provider.

The accepted trust model is:

1. Private AI Gateway verifies an AMD SEV-SNP attestation report from
   `https://<vm>:29343/cpu`.
2. The report's `report_data` binds (a) the SHA-256 of the served TLS X.509 cert
   in DER form and (b) a fresh NVIDIA NRAS nonce.
3. Private AI Gateway pins that TLS cert digest on every subsequent request to
   any port on the same hostname (attestation port, inference port).
4. The SEV-SNP launch measurement is recomputed from the published OVMF /
   kernel / initrd / rootfs hashes plus a kernel cmdline that embeds the
   SHA-256 of the served docker-compose, then compared with the 48-byte
   `Measurement` in the report.
5. NVIDIA NRAS evidence carrying the bound nonce is verified to confirm the
   attested GPU is genuine, non-debug, and matches the expected confidential
   computing posture.

The main conditions are:

- Pin the reviewed SecretVM `artifacts_ver` (e.g. `v0.0.26-beta.2`) as the
  accepted launch-digest set. Without an allowlist, any future entry SCRT adds
  to `secretvm-verify/.../sev.json` would be implicitly trusted.
- Pin the reviewed `secret-ai-caddy` image (or compose) hash for production
  traffic. The compose hash is bound to the launch digest, but the bound
  digest only proves "the running compose equals the one we measured," not
  "the compose we measured is the audited one."
- Treat single-VM cert renewal as a lease-invalidating event (the binding is
  to the full cert, not the SPKI).
- Document that the served-model identity (`gpt-oss:120b` over Ollama) is
  declared in the measured compose but not cryptographically tied to the
  pulled weights, the same caveat that applies to Chutes.

## Criteria Status

Passed:

- A SEV-SNP report from `/cpu` cryptographically binds the served TLS cert and
  a fresh GPU nonce. Verified live on 2026-05-22:
  - `report_data[0:32]` matched `sha256(DER(server_cert))`.
  - `report_data[32:64]` matched the `nonce` field in the `/gpu` response.
  - `signing_key = vcek`, `Debug Allowed = false`, `VMPL = 1`.
- The docker-compose YAML served at `/docker-compose` is bound to the SEV-SNP
  launch measurement through the kernel cmdline: see
  `secretvm-verify/python/src/secretvm/verify/workload.py:529-541`. The
  verifier recomputes the launch digest from `sev.json` + `sha256(compose)`
  and compares with the 48-byte `Measurement` field. This is a real
  compose-to-quote binding, not a "trust the served compose" claim.
- The GPU nonce is freshly generated at attestation time via `crypt-tool rand`
  and submitted to NVIDIA NRAS for live evidence retrieval; see
  `secret-vm-ops/scripts/secret-vm-functions.sh:704-728` and
  `secret-vm-ops/scripts/gpu-attest.py`.
- The attestation REST server reads compose, certs, and quotes from a
  pre-configured secure path tree and does not expose `/env`,
  `/public-key-*`, `/quote`, or VM-internal endpoints. Confirmed by live probe
  against the deployed instance.
- The TLS cert was issued by a publicly trusted CA (ZeroSSL ECC DV), but the
  binding is to the cert digest in the SEV-SNP report, not to the CA chain,
  so WebPKI is not part of the trust root.
- Token metering posts only token counts and a hashed API key to the
  configured metering endpoint; see
  `secret-ai-caddy/secret-reverse-proxy/token_meter.go` and
  `token_reporter.go:176-177`. No request or response bodies leave the VM
  through the metering path.
- API-key validation runs inside the VM with a master-key file fast path and
  a Secret Network contract query for non-master keys; see
  `secret-ai-caddy/secret-reverse-proxy/validators/api_key_validator.go`.
  Keys are never sent to the contract in plaintext — only their hash.

P0 adapter requirements:

- Verify the SEV-SNP report on `/cpu` and the GPU NRAS evidence on `/gpu`,
  then pin the bound full-cert SHA-256 (not just the SPKI) on every outbound
  HTTPS connection for the lease window.
- Recompute the SEV-SNP launch digest using the reviewed `sev.json` entry,
  including the kernel cmdline with `docker_compose_hash=<sha256(compose)>`,
  and require equality with the report's `Measurement`.
- Treat the SHA-256 of the live `/docker-compose` as part of the lease's
  evidence digest. Refuse to issue a lease if the served compose hash and the
  cmdline-embedded hash do not match.
- Reject a quote whose `Debug Allowed`, `VMPL != 1`, or `Family ID` indicates
  a non-prod template, and reject quotes with reported TCB strictly lower
  than the reviewed baseline.
- Require the NVIDIA NRAS evidence to verify against `nras.attestation.nvidia.com`
  with the same nonce that is in `report_data[32:64]`. Fail closed if NRAS
  rejects the evidence.

P0 TODOs before strict-release inclusion:

- Maintain an explicit allowlist of accepted SecretVM `artifacts_ver` values
  and accepted family templates (`prod-medium-sev`, `prod-large-sev`,
  `prod-2xlarge-sev`, etc.) tied to a reviewed
  `secretvm-verify/.../sev.json` revision. Do not trust new entries SCRT
  publishes after the reviewed revision until they are re-reviewed.
- Pin the reviewed `secret-ai-caddy` compose / image digest. Confirm the
  production compose does not enable verbose request-body DEBUG logging
  (`secret-ai-caddy/secret-reverse-proxy/log_request.go:13-52`) at a log
  level that persists user prompts to disk or remote journal sinks.
- Confirm the production VM does not ship system journal output off-VM.
  Caddy's `LogRequest` writes truncated request bodies at DEBUG, with
  `Authorization` redacted. That is in-trust-boundary, but only if journald
  output is not forwarded to a logging plane outside the SEV-SNP enclave.
- Define SCRT's release publication process for new measurements. The
  `secretvm-verify` SDK ships measurement tables, but new entries can land
  without an external pre-publication step. Strict verifiers must fail
  closed on entries published after the reviewed pin.
- Decide policy for cert renewal. ZeroSSL DV certs are 90-day. The bound
  digest changes on every renewal, so the adapter must refresh the lease at
  or before cert rotation and must not silently fall back to plain HTTPS if
  the new cert is not yet bound by a new attestation.

P1 TODOs:

- Record the bound TLS cert digest, `Measurement`, `Family ID`, and
  `artifacts_ver` in receipt provider claims.
- Add a negative test that swaps `docker-compose.yaml` content (or its served
  bytes) and confirms the adapter refuses to issue a lease because the
  cmdline-embedded compose hash no longer matches.
- Add a live test that pulls `/cpu`, `/gpu`, and `/docker-compose` over a
  TLS connection pinned to a deliberately wrong cert digest and confirms the
  connection is rejected.
- Treat any value of `Host Data`, `Family ID`, `Image ID`, or `Author Key
  Digest` that does not parse into a known SecretVM family as a verifier
  failure. The live instance reports `Host Data = 00..00` and `Image ID`
  encoding `artifacts_ver` as ASCII; both are required.

## Workload Identity And Binding

SecretAI's workload identity is the single SecretVM instance. There is no
upstream router and no per-request key rotation.

Report-data layout, confirmed live and in source:

```text
report_data[0:32]  = sha256(server_cert_der)
report_data[32:64] = gpu_nras_nonce
```

Source: `secret-vm-ops/scripts/secret-vm-functions.sh:704-728`. The shell
helper computes the SHA-256 fingerprint with `openssl x509 -fingerprint
-sha256`, generates a fresh 32-byte nonce with `crypt-tool rand`, concatenates
them, and passes the result to `attest-tool attest`. The same script then
invokes `gpu-attest` to call NVIDIA NRAS with the same nonce.

Live measurement, 2026-05-22:

```text
TLS cert SHA-256 = 2bcff5c195714a196e7aa18e01fdc047fa922b45b7e15342edd6550c11aefe38
GPU nonce        = cbd79ce91c937e7d1d71322dfbe381bc543777ca36e9f6496dc8a1bfb4bff895
report_data[ 0:32] match: True
report_data[32:64] match: True
Measurement (48B) = 69752b8caaf3fd13ff3fccaf311b76327368b868365ccad7297f3469949c9421eae8f42ce767fa3e3077138214cf3b98
host_data (32B)   = 0000000000000000000000000000000000000000000000000000000000000000
```

The TLS private key is generated inside the VM at first boot (EC P-256, see
`secret-vm-ops/setup/src/cert.rs:47-52`) and never leaves the enclave. The
public cert is obtained via ACME against ZeroSSL (`cert.rs:84-92`); the
keypair lifecycle stays inside SEV-SNP, the CA is just a name-binding
formality. The accepted binding is the in-quote cert digest, not the CA
chain.

Channel binding for Private AI Gateway is `tls_cert_sha256` (full DER), not
`tls_spki_sha256`. The two differ for SecretAI (`spki_sha256 =
dfd0e9d0801904a82b9cd1b6bc8a492e3d494f2e7292a21ccef5d3f3f4617370` does not
match `report_data[0:32]`). Adapter rule: digest the entire `CertificateDer`
that the peer presents and compare to the bound value. Renewal invalidates
the lease.

## Compose Measurement

The SEV-SNP launch `Measurement` is the standard AMD GCTX digest plus VMSA
pages. It does not natively carry an RTMR-like accumulator. SecretVM binds
the compose hash by appending it to the guest kernel cmdline before launch:

```python
# secretvm-verify/python/src/secretvm/verify/workload.py:529-541
compose_hash = hashlib.sha256(docker_compose_yaml.encode("utf-8")).hexdigest()
cmdline = (
    f"console=ttyS0 loglevel=7 "
    f"docker_compose_hash={compose_hash} "
    f"rootfs_hash={rh}"
)
if df_sha:
    cmdline += f" docker_additional_files_hash={df_sha}"
return _sev_calc_measurement(entry, vcpus, cmdline) == quote_measurement
```

The cmdline is hashed into the GCTX through the `SNP_KERNEL_HASHES`
(section_type 0x10) page during launch. A verifier therefore recomputes the
expected launch digest from the published `kernel_hash`, `initrd_hash`,
`rootfs_hash`, `ovmf_hash`, `ovmf_sections`, `vcpu_type`, `sev_es_reset_eip`,
plus the vCPU count from `family_id`, plus this cmdline string.

This is sound and matches the design intent: compose change ⇒ cmdline change
⇒ launch digest change. The adapter must use the same canonicalization SCRT
uses (raw bytes, no YAML re-serialization, see comment "raw SHA256 of
compose content (no YAML normalization)" at `workload.py:528`).

`Host Data` is zero on the deployed instance. `Image ID` encodes the
artifacts version as ASCII (e.g. `v0.0.26-beta.2`). `Family ID` encodes
`"{vm_type}-{template}-sev"` (e.g. `prod-medium-sev`) and selects the vCPU
count via `_SEV_VCPU_MAP`. The adapter should refuse any quote whose
`Family ID` does not parse into a known prod template.

The published measurement table is
`secretvm-verify/python/src/secretvm/verify/data/sev.json` (12 entries at the
reviewed commit) and `tdx.csv` for the TDX flavor. Adapter behavior must be:

```text
verified gateway lease
+ reviewed sev.json entry (pinned by artifacts_ver, vm_type)
+ sha256(live /docker-compose) embedded in cmdline
+ recomputed launch digest == Measurement
```

The TDX flavor (`tdx.csv`) is not used by `secretai-jedi.scrtlabs.com` but
applies to other SecretVMs SCRT may operate, including any future SecretAI
deployment on TDX hosts. The same registry-pin discipline applies to TDX.

## GPU Attestation Chain

`/gpu` returns the NVIDIA NRAS verifier evidence shape:

```json
{
  "nonce": "<32-byte hex>",
  "arch": "HOPPER",
  "evidence_list": [{"certificate": "<base64 pem>", "evidence": "<base64 NRAS evidence>"}]
}
```

Source: `secret-vm-ops/scripts/gpu-attest.py`. The SecretVM uses
`nv_attestation_sdk` to fetch evidence with the verifier-provided nonce,
then writes the evidence list plus a JWT token to disk. The bound nonce
exactly equals `report_data[32:64]` in the CPU quote, so an adapter that
verifies the GPU evidence against the bound nonce gets joint freshness for
both attestations from a single round trip.

The deployed `/gpu` response carries a single HOPPER evidence entry. The
adapter should submit it to NVIDIA NRAS (or verify the included JWT) using
the same nonce, and reject if either step fails. Pre-fetched evidence
cannot be replayed: the bound nonce is regenerated on every
`secret-vm-functions.sh` attestation cycle.

## Catalog And Model Identity

There is no provider model catalog. SecretAI's deployed instance serves one
model identity declared in the docker-compose: `ollama/ollama:0.20.7`
loading `gpt-oss:120b`.

The compose pins the Ollama image with a tag, not a digest, and Ollama can
in principle pull arbitrary models at runtime. The measured compose
constrains the entrypoint and environment, but does not constrain the model
tag that Ollama eventually serves over `/v1/chat/completions`. Same posture
as Chutes: the lease can honestly claim "this VM is the reviewed SecretAI
build running the reviewed compose"; it cannot honestly claim "the served
weights are exactly `gpt-oss:120b`."

Adapter rule for receipts:

```text
provider_claim = {
  vm_artifacts_ver: "...",
  family: "prod-medium-sev",
  compose_sha256: "...",
  declared_model: "gpt-oss:120b"   // from compose, not signed weights
}
```

Adapter rule for outbound model id:

```text
Configured upstream model id -> declared compose model.
No catalog probe, no fallback, no alias resolution.
```

## Privacy Boundary

Request bytes never leave the SEV-SNP VM through reviewed code paths.

Findings:

- `secret-ai-caddy/secret-reverse-proxy/log_request.go:13-52` slurps the
  request body, redacts `Authorization`, truncates to `maxBody`, and writes
  the dump to `caddy.Log().Debug(...)`. Body is plaintext in the in-VM
  journal at DEBUG level. Production runtime policy must keep the in-VM
  journal inside the trust boundary (no syslog forward to off-VM hosts).
- `secret-ai-caddy/secret-reverse-proxy/metering/request_body_handler.go`
  reads the body to extract token-eligible content for in-VM tokenization
  only. The content is not exported.
- `token_reporter.go:176-177` and the `x402/portal_client.go` paths send
  only `(hashed_api_key, model, input_tokens, output_tokens)` to the
  configured metering / portal endpoints.
- API-key validation in `validators/api_key_validator.go:79-100` checks an
  in-VM master-key list first, then queries a Secret Network LCD endpoint
  for contract-backed subscription state; only hashes are submitted.
- The attestation REST server does not expose `/env`,
  `/public-key-ed25519`, `/public-key-secp256k1`, or any path beyond the
  documented `/cpu`, `/gpu`, `/docker-compose`, `/self` (live probed
  2026-05-22; all other paths returned 404).

The compose declares `SECRETVM_ENV_PATH=/mnt/secure/docker_wd/usr/.env`,
which the attestation server can read internally but does not serve.

## Privacy P0 Items

- Confirm production Caddy log level is not DEBUG for the body-logging
  path, or that DEBUG output is kept on-disk inside the SEV-SNP measured
  rootfs and not forwarded.
- Confirm the metering endpoint and x402 portal URL configured in the
  production compose are themselves under the operator's control, since
  the (hash, token-count) telemetry leaks usage metadata. This is not a
  content leak, but it is an information-flow item for the receipt and
  cost-model story.
- Note the API-key contract query goes to a public Secret Network LCD
  endpoint. Only hashes are sent, but the LCD operator sees the timing
  and the hash. Acceptable as a metadata leak, but worth recording.

## Inference Surface

Live probe 2026-05-22 found inference on `https://<vm>:21434`:

```text
GET /v1/models       -> 401 (no Authorization)
GET /api/tags        -> 401
GET /api/version     -> 401
```

`secret-ai-caddy` mediates auth before forwarding to Ollama on the
container network. Ollama exposes both its native `/api/*` surface and the
OpenAI-compatible `/v1/chat/completions`, `/v1/models`,
`/v1/embeddings`, `/v1/completions` shapes; the adapter should use the
`/v1/*` paths.

Other ports advertised in the compose (`25435` TTS, `25436` STT, `18800`
Solidity LLM) timed out from the network observed during review and are
out of scope for the chat-completions integration.

## Release And Measurement Updates

The `secretvm-verify` SDK ships allowlist tables:

- `secretvm-verify/python/src/secretvm/verify/data/sev.json` (12 entries
  at the reviewed commit).
- `secretvm-verify/python/src/secretvm/verify/data/tdx.csv` (multiple
  entries).

Each entry includes `vm_type`, `template_name`, `artifacts_ver`,
`kernel_hash`, `initrd_hash`, `rootfs_hash`, `ovmf_hash`, `ovmf_sections`,
`sev_es_reset_eip`, `sev_hashes_table_gpa`, `vcpu_type`. This is enough
for a verifier to recompute the launch digest.

What is missing for strict-release inclusion:

- A separate publication channel that announces new entries before they
  land in the SDK. Today the SDK is the publication channel, so a verifier
  that auto-pulls the latest SDK is effectively pinned to "whatever SCRT
  pushed last."
- A signed build-provenance attestation tying `artifacts_ver` to a
  reproducible source revision.

Strict mode for SecretAI should pin a reviewed `sev.json` revision (e.g.
the `e80a54ffe8aae17e4103e6b62ed7f51f3eeb1801` commit) and require an
explicit re-review when the `secret-vm-ops` and `secret-ai-caddy`
repositories cut new releases.

## Lease Lifecycle

Recommended lease shape for the SecretAI adapter:

```json
{
  "result": "verified",
  "provider": "secret-ai",
  "model_id": "<configured-canonical>",
  "verifier_id": "secretvm-verify/<commit>",
  "evidence_digest": "sha256:<cpu_quote || gpu_evidence || compose>",
  "verified_at": "...",
  "expires_at": "...",
  "channel_bindings": [
    {
      "type": "tls_cert_sha256",
      "origin": "https://<vm>:21434",
      "cert_sha256": "<report_data[0:32]>"
    }
  ],
  "provider_claims": {
    "vm_artifacts_ver": "v0.0.26-beta.2",
    "family": "prod-medium-sev",
    "compose_sha256": "...",
    "gpu_arch": "HOPPER",
    "declared_model": "gpt-oss:120b"
  }
}
```

Refresh notes:

- Refresh must re-fetch `/cpu`, `/gpu`, `/docker-compose` and recompute the
  launch digest. Treat a TLS cert rotation as a forced refresh.
- A failed refresh must not replace a valid old lease, but traffic must
  stop when the old lease expires. The TLS pin is on the full cert digest,
  so a partial refresh that loses the bound cert breaks the channel
  immediately.
- The bound GPU nonce is single-use freshness for the attestation, not for
  individual requests. There is no per-request E2EE handshake, unlike
  Chutes.

## Required Adapter Behavior

The SecretAI adapter must:

- Fetch `/cpu`, `/gpu`, `/docker-compose` over a TLS connection that, after
  the first lease establishment, is pinned to the bound cert SHA-256.
- Verify the SEV-SNP signature chain (VCEK → AMD root) and reject any
  quote where `Debug Allowed != false`, `VMPL != 1`, or TCB version is
  lower than the reviewed baseline.
- Parse the `Family ID` and `Image ID` ASCII fields and refuse to issue a
  lease if either does not match a pinned production template /
  `artifacts_ver`.
- Recompute the SEV-SNP launch measurement from the pinned `sev.json`
  entry plus the cmdline embedding `sha256(live compose)`, and require
  equality.
- Verify the NVIDIA NRAS evidence using the nonce in
  `report_data[32:64]`. Fail closed if NRAS rejects the evidence.
- Refuse to use SPKI binding even though SPKI is the usual baseline:
  SecretAI binds the full cert digest, not the SPKI digest.
- Use only `/v1/chat/completions`, `/v1/models`, `/v1/embeddings`,
  `/v1/completions` paths over port `21434`. Do not depend on the native
  `/api/*` Ollama surface.
- Strip any operator-configured `Authorization` header rewriting at the
  forwarding boundary; SecretAI's Caddy expects the SecretAI API key in
  the `Authorization` header.
- Record the bound TLS cert digest, `Measurement`, `Family ID`,
  `artifacts_ver`, and `compose_sha256` in receipts.

## Negative Checks

The provider test suite must include:

- Replace the TLS cert digest in the bound report and confirm the adapter
  rejects the forwarded request (channel-binding mismatch).
- Modify the served `/docker-compose` bytes after the lease and confirm the
  next refresh fails closed because the cmdline-embedded compose hash no
  longer matches.
- Submit an old `/gpu` response with a stale nonce and confirm the adapter
  rejects the lease (`gpu_nonce != report_data[32:64]`).
- Submit a quote with `Debug Allowed = true` and confirm rejection.
- Submit a quote whose `Family ID` parses to a `dev`-type template and
  confirm rejection in strict mode.
- Submit a quote whose `artifacts_ver` is not in the pinned `sev.json`
  allowlist and confirm rejection.
- Force a verifier failure on a refresh and confirm the previous lease is
  not replaced and that traffic stops when it expires.

## Open Questions

- Will SCRT publish per-release source-provenance attestations for
  `artifacts_ver` values, or is reproducible build-from-source the
  intended verification path?
- Is the production `secret-ai-caddy` compose's log level set such that
  `LogRequest`'s body dumps are suppressed, or does the production journal
  contain truncated request bodies under DEBUG?
- Does the production VM use the same metering endpoint and x402 portal as
  the open-source defaults, or operator-controlled equivalents under the
  same trust boundary?
- Is the registry of accepted `template_name` values for SecretAI's
  production fleet a subset of the `sev.json` entries, or does SCRT plan
  to run only `prod-*-sev` templates for SecretAI?
- Does Ollama's runtime model pull behavior get further constrained by
  SCRT-side tooling, or is the only constraint the compose's declared
  model id?
- Can SCRT add a model-scoped attestation echo (the request returns the
  compose hash alongside the response) so Private AI Gateway can put
  served-compose evidence into the per-request receipt without a separate
  `/docker-compose` fetch?
