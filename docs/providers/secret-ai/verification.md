# SecretAI Verification

This reference is for Private AI Gateway operators who configure a direct
SecretAI SecretVM origin. The gateway verifies the CPU TEE, NVIDIA GPU,
measured SecretVM workload, and inference TLS key before it forwards traffic.

## Configure a SecretAI origin

`accepted_workload_ids` is optional. When configured, a workload ID pins the CPU
technology, SecretVM environment, VM template, SecretVM artifacts version, and
exact SHA-256 digest of `/docker-compose`:

```text
secretvm:<cpu-type>:<environment>:<template>:<artifacts-version>:sha256:<compose-sha256>
```

The following unpinned JEDI configuration was verified live on 2026-07-21:

```json
[
  {
    "name": "secret-ai-jedi",
    "provider": "secret-ai",
    "base_url": "https://secretai-jedi.scrtlabs.com:21434",
    "models": {
      "secret-ai/gpt-oss-120b": "gpt-oss:120b"
    },
    "bearer_token": "<secret-ai-api-key>"
  }
]
```

To pin that exact measured workload, add:

```json
"accepted_workload_ids": [
  "secretvm:sev-snp:gpu_prod:4xlarge:v0.0.33:sha256:ea08d2b8a03bea1d3206286e50da41e437b3fd4a9e6e3415a0d6169f05bb7cf2"
]
```

The following RYTN workload was also verified live on 2026-07-21:

```text
secretvm:tdx:prod:4xlarge_256gb_gpu:v0.0.33:sha256:3a013b14abfacd03515ca3c6ae2d9d45d489ce8692c7fa3dff7f73837e3c8b1a
```

A SecretAI rollout that changes the VM artifacts or compose bytes produces a
different workload ID. An unpinned configuration reports the new ID and
continues after all mandatory checks pass. A pinned configuration rejects it
until an operator updates the allowlist. The embedded AMD TCB minimum was
verified against JEDI on 2026-07-21.

## Verification algorithm

SecretAI support is embedded in `private-ai-verifier` and reports verifier ID
`private-ai-verifier/secret-ai/v1`. It uses `secretvm-verify==0.12.0`
internally; the exact wheel and its bundled artifact registries are pinned by
`uv.lock`.

For each verification, the adapter:

1. Requires a root HTTPS origin with no userinfo, query, fragment, or path.
2. Fetches `/cpu`, `/gpu`, and `/docker-compose` from that origin through normal
   WebPKI TLS. Each endpoint must return `text/plain`; response sizes are bounded.
3. Opens an independent WebPKI TLS connection, proves live possession of the
   inference private key through the TLS handshake, and computes
   `SHA256(SubjectPublicKeyInfo)` from the leaf certificate.
4. Accepts `/cpu` only as a raw hex TDX quote or base64 SEV-SNP report, then
   calls the corresponding official SecretVM raw-quote verifier directly. URL
   autodetection is never used. AMD verification runs in strict mode and does
   not accept stale KDS collateral or a guest policy that permits an unverified
   migration agent.
5. Requires TDX DCAP status to be exactly `UpToDate`. For SEV-SNP, requires the
   signed `reported_tcb` components to meet the verifier's embedded minimum:
   boot loader 10, TEE 0, SNP 23, and microcode 88.
6. Requires the 64-byte CPU `report_data` to contain the inference TLS SPKI
   digest in bytes 0 through 31.
7. Requires `/gpu` to contain a 32-byte nonce equal to CPU `report_data` bytes
   32 through 63, then requires NVIDIA NRAS verification to pass. The signed
   NRAS overall result must be the boolean `true`; its platform nonce and every
   signed per-GPU nonce-match result must agree. GPU model claims come only from
   the signed per-GPU reports.
8. Reconstructs the measured SecretVM workload from the exact
   `/docker-compose` response bytes and the registry bundled with the pinned
   verifier package.
9. Requires one unique registry match in the `prod` or `gpu_prod` environment.
   If `accepted_workload_ids` is configured, also requires an exact policy match.
10. Emits a `tls_spki_sha256` channel binding. The Rust forwarding path checks
   that SPKI against the actual inference TLS connection before sending the
   request.

TDX verification matches `MRTD` and `RTMR0` through `RTMR2` to a pinned registry
entry, then replays `RTMR3` with the exact compose bytes. SEV-SNP verification
recomputes the launch digest for each pinned registry entry and supported vCPU
profile. This exhaustive SEV path supports deployments whose signed `family_id`
field is zero, while still deriving one unique identity from the launch
measurement.

The adapter does not refresh the workload registry from GitHub at runtime. It
also does not extract compose text from HTML or normalize YAML. Those behaviors
would change the trust root or measured bytes outside the reviewed gateway
release.

## Verified claims

| Claim | Result |
| --- | --- |
| TEE attested | Asserted from a verified TDX or SEV-SNP report bound to the inference SPKI. |
| Platform TCB current | TDX must be `UpToDate`. SEV-SNP must meet the embedded minimum; its absolute freshness remains unknown. |
| Serving software known good | Unknown by default; asserted when the uniquely measured workload matches an optional operator pin. |
| OS known good | Asserted only for a matched `prod` or `gpu_prod` registry entry. |
| GPU attested | Asserted after NRAS verification and an exact GPU nonce match against CPU `report_data`. |
| Model weights provenance | Unknown. The measured compose names the model and pins container images, but does not hash downloaded model weights into the launch measurement. |

The session scope is `router`: one SecretVM TLS origin can route multiple models
behind the same attested workload and SPKI.

## Trust boundaries and limitations

- `/cpu`, `/gpu`, and `/docker-compose` are lease evidence. SecretAI does not
  bind a caller-generated nonce into a new CPU quote for every gateway
  verification.
- Freshness comes from live TLS proof of possession of the SPKI-bound private
  key, not from a verifier nonce in the CPU report. The accepted SecretVM
  key-custody model generates that private key inside the measured VM and stores
  it on the secure filesystem. If an operator can export or inject that key, old
  evidence becomes replayable and the provider no longer meets this trust model.
- The SPKI binding proves that forwarding reaches the TLS key named by the CPU
  report. It does not prevent software inside the measured workload from logging
  prompts or responses.
- An accepted workload ID is an optional operator policy decision. Review the
  full compose, image digests, SecretVM artifact release, endpoint exposure, and
  provider logging behavior before adding a pin.
- The SEV-SNP minimum is release policy embedded in the verifier. Raise it in a
  reviewed gateway release when the AMD security baseline changes; a lower
  signed report fails closed.
- A registry or verifier upgrade changes the verification trust root. Upgrade
  `secretvm-verify` and `uv.lock` deliberately, then rerun the live and hermetic
  tests.
- Workloads that extend TDX RTMR3 with a docker-files archive are not currently
  accepted because SecretAI does not expose that archive digest as evidence.
  Such workloads fail closed until the evidence contract and workload identity
  include the additional measured input.

## Test the adapter

Run the offline policy and bridge checks:

```bash
uv run python tests/provider_verifier/secret_ai_soundness.py
cargo test secret_ai
```

A successful result has `result: "verified"`, `attested_scope: "router"`, one
`tls_spki_sha256` channel binding, and the measured workload ID in
`provider_claims.workload_id`. A configured matching pin is also reported as
`provider_claims.accepted_workload_id`.
