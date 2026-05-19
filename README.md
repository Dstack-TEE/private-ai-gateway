# private-ai-gateway

Developer-preview Rust implementation of the **Attested Confidential Inference
(ACI)** aggregator service.

The protocol it speaks is the draft ACI specification proposed in
[`Dstack-TEE/dstack#694`](https://github.com/Dstack-TEE/dstack/pull/694).
This crate is the program a trusted workload launcher (separate repo) can
fetch, install, and run inside a dstack v2 application VM.

## Status

`0.1.0` - developer preview. Production-blocking work is explicit below.

| Surface | Status |
| --- | --- |
| Canonical JSON (RFC 8785 subset) | done |
| Workload identity / keyset digests | done |
| Attestation report (assembly + endorsement) | done |
| Inference receipts (event log, signing) | done |
| Non-streaming `POST /v1/chat/completions` forwarding | done |
| `POST /v1/completions` forwarding | done |
| Streaming chat/completions forwarding | done |
| `GET /v1/attestation/report` | done |
| `GET /v1/receipt/{chat_id}` | done |
| `GET /v1/signature/{chat_id}` alias | done |
| Upstream verification fail-closed by default | done |
| ECDSA-secp256k1 65-byte recoverable receipt sig | done |
| Receipt owner auth + retained body endpoint | done (in-memory; retention defaults to 0) |
| E2EE-header fail-closed guard | done |
| dstack SDK quoter over HTTP(S) or Unix socket | done |
| dstack KMS-backed identity + receipt + E2EE keys | done |
| Client-facing E2EE v2 termination | done for chat/completions |
| Client-facing E2EE v2 streaming | done for chat/completions |
| vLLM-proxy-compatible ECDSA v1/v2 and Ed25519/X25519 E2EE | done for chat/completions |
| `/v1/models` upstream proxying | done |
| `/v1/metrics` aggregator-owned Prometheus metrics | done |
| Runtime upstream config file + admin API | done |
| Per-upstream verifier | done for ACI/DCAP, Tinfoil, NEAR AI gateway, and Chutes E2EE-key bindings |
| Chutes provider transport | done for buffered and streaming E2EE over `/e2e/invoke` |
| Public receipt log | not done |
| Replica-stable identity (KMS-released keys) | done for configured dstack key paths |

The binary has no ephemeral-key or stub-quote startup path. It loads
identity, receipt-signing, and E2EE keys from dstack KMS through the Rust
dstack SDK, and it uses the same SDK for TDX quotes.

## Layout

```
src/
  lib.rs
  main.rs               // binary entrypoint
  dstack.rs             // dstack SDK KMS key provider + quote provider
  aci/
    canonical.rs        // JCS subset, UTF-16 key sort, sha256 helpers
    types.rs            // wire structs (WorkloadKeyset, Receipt, ...)
    identity.rs         // workload_id, keyset digest, report_data
    keys.rs             // KeyProvider / Quoter traits and signature verifiers
    receipt.rs          // ReceiptBuilder + signing-bytes function
    upstream.rs         // UpstreamBackend trait + OpenAI-compatible client
  aggregator/
    service.rs          // AciService: report, forward, receipt store
  http/
    app.rs              // axum router for the ACI/OpenAI-compatible endpoints

entrypoint.sh           // aggregator-owned entry script the launcher exec's
scripts/
  phala_multi_upstream_smoke.sh // deploys two upstream ACI CVMs + one router CVM and asserts routing receipts
deploy/                 // launcher .conf, runtime .env, dstack compose example
  README.md             // launcher wiring and deployment notes

tests/
  canonical.rs          // JCS stability, UTF-16 sort, float rejection
  identity.rs           // workload_id excludes subject, keyset digest includes it
  receipt.rs            // event ordering, finalization, signing bytes
  ecdsa_recoverable.rs  // §9.4 65-byte recoverable, reject 64-byte, no double hash
  service.rs            // fail-closed defaults, X-Upstream-Verification: none
  http.rs               // end-to-end report / chat / receipt
  aggregator_scenarios.rs // full ACI aggregator happy/error path scenarios
  auth_and_retention.rs // receipt owner auth, retained body expiry, ACI headers
  aci_service_surface.rs // implemented surfaces plus ignored future specs
  entrypoint.rs         // shellcheck-lints and shape-checks entrypoint.sh
  smoke_scripts.rs      // shellcheck + invariant checks for scripts/
```

## Launcher wiring (`entrypoint.sh`)

This repo is designed to be launched by
[`trusted-workload-launcher`](https://github.com/Dstack-TEE/dstack-examples/tree/main/trusted-workload-launcher).
The launcher pulls the repo at a pinned commit, `cd`s into
`REPO_SUBDIR=private-ai-gateway`, exports the `CHILD_ENV_FILE`, and
runs an aggregator-owned entry script.

**Ownership boundary.** The launcher is generic and build-system agnostic;
it does not know we are written in Rust. `entrypoint.sh` is owned by this
aggregator, and everything past `bash entrypoint.sh` — install, build,
run — lives here. The launcher config stays minimal (`REPO_URL`,
`COMMIT_SHA`, `REPO_SUBDIR`, `WORK_DIR`, `CHILD_ENV_FILE`); there is no
`INSTALL_CMD` and no `RUN_CMD`.

> **Required launcher-side follow-up:** the current
> `trusted-workload-launcher` versions that still hardcode the legacy default
> legacy entry name `tee-launch.sh` must be updated to look for `entrypoint.sh` (or a
> configurable entry name). Until then, this slice does **not** run end-to-end
> on the unmodified launcher in default mode. See `deploy/README.md` →
> "Required launcher-side follow-up".

What `entrypoint.sh` does (once the launcher invokes it):

1. If `cargo` is not on `PATH`, this aggregator installs a Rust toolchain
   via `apt-get install -y --no-install-recommends ca-certificates rustup`
   + `rustup default stable`. **This is an aggregator implementation
   choice for the first slice**, not a launcher capability. Production
   should publish a Rust-capable aggregator image (see `deploy/README.md`
   pattern B) so the toolchain is covered by an aggregator-owned image
   digest.
2. Runs `cargo build --release --locked --bin private-ai-gateway`. The
   `--locked` flag means a build that would change `Cargo.lock` is a hard
   failure, not silent dependency drift.
3. `exec`s the built binary.

See `deploy/README.md` for the launcher `.conf`, the `CHILD_ENV_FILE`
runtime env, the dstack compose example that puts both behind
`compose_hash`, and the Rust-capable aggregator image recipe.

## Environment variables

Use the `PRIVATE_AI_GATEWAY_*` prefix for runtime configuration. The binary
also accepts the older `DSTACK_LLM_ROUTER_*` names as compatibility aliases;
the `PRIVATE_AI_GATEWAY_*` value wins when both are set.

| Setting | Name |
| --- | --- |
| Bind address | `PRIVATE_AI_GATEWAY_BIND` |
| Upstream config file | `PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_PATH` |
| Admin API bearer token | `PRIVATE_AI_GATEWAY_ADMIN_TOKEN` |
| Source-provenance repo URL | `PRIVATE_AI_GATEWAY_REPO_URL` |
| Source-provenance commit | `PRIVATE_AI_GATEWAY_REPO_COMMIT` |
| Body retention seconds | `PRIVATE_AI_GATEWAY_BODY_RETENTION_SECONDS` |
| Receipt TTL seconds | `PRIVATE_AI_GATEWAY_RECEIPT_TTL_SECONDS` |
| TLS certificate paths, comma-separated | `PRIVATE_AI_GATEWAY_TLS_CERT_PATHS` |
| TLS SPKI SHA-256 digests, comma-separated | `PRIVATE_AI_GATEWAY_TLS_SPKI_SHA256` |
| Upstream verifier mode: `none`, `preverified`, `aci-dcap` | `PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER` |
| Accepted upstream workload IDs, comma-separated | `PRIVATE_AI_GATEWAY_UPSTREAM_ACCEPTED_WORKLOAD_IDS` |
| Accepted upstream image digests, comma-separated | `PRIVATE_AI_GATEWAY_UPSTREAM_ACCEPTED_IMAGE_DIGESTS` |
| Accepted upstream dstack KMS root public keys, comma-separated | `PRIVATE_AI_GATEWAY_UPSTREAM_DSTACK_KMS_ROOT_PUBLIC_KEYS` |
| Upstream verifier PCCS URL | `PRIVATE_AI_GATEWAY_UPSTREAM_PCCS_URL` |
| Upstream verifier cache seconds | `PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER_CACHE_SECONDS` |
| Upstream TCP/TLS connect timeout seconds | `PRIVATE_AI_GATEWAY_UPSTREAM_CONNECT_TIMEOUT_SECONDS` |
| Upstream read idle timeout seconds | `PRIVATE_AI_GATEWAY_UPSTREAM_READ_TIMEOUT_SECONDS` |
| Upstream verifier request timeout seconds | `PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER_REQUEST_TIMEOUT_SECONDS` |
| dstack SDK endpoint | `PRIVATE_AI_GATEWAY_DSTACK_ENDPOINT` |

Provider-owned verifier bridges also read `PRIVATE_AI_VERIFIER_DIR` when they
need the local `private-ai-verifier` checkout. If unset in this monorepo, the
aggregator uses the sibling `../private-ai-verifier` path. Chutes credentials
and E2EE tuning are upstream config fields, not deployment env vars:
`bearer_token`, `chutes_e2ee_api_base`, `chutes_chute_ids`,
`chutes_e2ee_discovery_rounds`, and
`chutes_e2ee_discovery_interval_seconds`. The Rust adapter passes those values
to the verifier bridge internally.

Prefer `PRIVATE_AI_GATEWAY_TLS_CERT_PATHS`: the aggregator reads the mounted
leaf certificate, computes `sha256(SPKI)`, and publishes that digest in the
attested keyset. `PRIVATE_AI_GATEWAY_TLS_SPKI_SHA256` remains for manual or
test deployments. Set only one of the two.

`PRIVATE_AI_GATEWAY_DSTACK_ENDPOINT` accepts an HTTP(S) endpoint or a Unix
socket endpoint such as `unix:/var/run/dstack.sock`. If unset, the dstack SDK
uses `/var/run/dstack.sock` or `DSTACK_SIMULATOR_ENDPOINT`. For local testing
with an SSH-forwarded CVM socket, use
`unix:/tmp/aci-dstack-sock-dev.dstack.sock`. The older
`PRIVATE_AI_GATEWAY_DSTACK_QUOTER_URL` name is still accepted as a compatibility
alias.

The default upstream-verification mode is `none`, while the request path is
fail-closed by default. `aci-dcap` is only for upstreams that expose the ACI
attestation report shape on dstack. Configure it with at least one accepted
upstream workload ID or image digest, and the accepted dstack KMS root public
key. The verifier fetches the upstream's `/v1/attestation/report`, validates
the ACI workload/keyset binding, verifies the embedded Intel DCAP quote through
`dcap-qvl`, replays the dstack event log against the quote's RTMR3, verifies
the identity key's dstack KMS signature chain to the configured KMS root, and
caches a successful result for 300 seconds unless overridden.
Provider adapters are Rust implementations, not configured shell commands. The
adapter owns the provider-specific transport path and may outsource
attestation verification to provider-owned verifier logic. The call is selected
by the Rust adapter, not by upstream config. Tinfoil uses the
`private-ai-verifier` bridge and returns the TLS SPKI bound in the Tinfoil
attestation document. NEAR AI requests its report with TLS fingerprint binding
enabled and returns that SPKI once the dstack verification dependency is
available. Chutes verifies the E2EE public key against TDX `report_data`,
Intel DCAP status, Chutes' public measurement profiles, and NVIDIA NRAS nonce
binding using the upstream config `bearer_token`. Its backend then fetches a
live nonce/key batch, selects only an instance whose E2EE public key matches the
verified binding, encrypts the OpenAI JSON body with Chutes' ML-KEM-768 +
HKDF-SHA256 + ChaCha20-Poly1305 transport, and decrypts buffered or streaming
responses before the receipt pipeline hashes them. The OpenAI-compatible backend
enforces `tls_spki_sha256` and `tls_certificate_sha256` bindings against the
actual upstream HTTPS handshake. `preverified` is only for explicit
out-of-band trust during bring-up.

Upstream forwarding defaults to a 10 second connect timeout and a 600 second
read idle timeout. The read timeout is not a total generation deadline; for
streaming responses it bounds how long the aggregator waits between upstream
chunks. Upstream ACI/DCAP verification uses the same connect timeout and a 60
second total verification timeout by default, covering report fetch, collateral
fetch, and quote checks. The timeout env vars set global defaults; per-upstream
config can override them with
`connect_timeout_seconds`, `read_timeout_seconds`, and
`verifier_request_timeout_seconds`. Chutes verification does live evidence,
DCAP, and NRAS checks; configure a higher per-upstream verifier timeout if the
default is too low for the selected chute.

The aggregator prewarms upstream verification at startup. It also proactively
refreshes cached verification before expiry; by default the refresh loop runs at
the verifier cache TTL minus 60 seconds, so the normal 300 second cache
refreshes every 240 seconds. If multiple upstreams configure different positive
refresh intervals, the loop uses the shortest active interval. External provider
verifier refresh keeps the current good cache entry while the new evidence is
fetched, so user requests can continue using the previous verified identity
during refresh. Set an upstream's `verification_refresh_seconds` to `0` to skip
that upstream during proactive verifier refresh.

Provider session material refreshes every 45 seconds by default for adapters
that have session material today. Set an upstream's `session_refresh_seconds`
to `0` to disable that loop. For Chutes, session refresh is lightweight: it
reuses cached verified E2EE key bindings and only refills single-use invocation
nonces, so user traffic does not have to wait for nonce discovery when the pool
is low or expired.

The aggregator has one upstream config file. Set
`PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_PATH` to its path; if unset, the default is
`/var/lib/private-ai-gateway/upstreams.json`. A missing, empty, or whitespace-only
file is valid and means no upstreams are configured yet. The file contains a
JSON array:

```json
[
  {
    "name": "gpu-a",
    "provider": "aci-dcap",
    "base_url": "https://gpu-a.example",
    "models": {
      "public-model-a": "upstream-model-a"
    },
    "accepted_workload_ids": ["aci:workload:..."],
    "accepted_dstack_kms_root_public_keys": ["02..."],
    "connect_timeout_seconds": 10,
    "read_timeout_seconds": 600,
    "verifier_request_timeout_seconds": 60,
    "verification_refresh_seconds": 240
  }
]
```

`provider` defaults to `openai-compatible`. Supported values are
`openai-compatible`, `aci-dcap`, `chutes`, `tinfoil`, and `near-ai`. The
non-default providers select concrete Rust verifier adapters; they do not
expose a configurable verifier command.

The public model id is what clients send and what `/v1/models` returns. The
aggregator rewrites it to the upstream model id before upstream verification,
forwarding, and receipt hashing. Per-upstream verifier fields override the
global `PRIVATE_AI_GATEWAY_UPSTREAM_ACCEPTED_*` settings when present.
Chutes upstreams should put the provider API key in `bearer_token`. Optional
Chutes fields are `chutes_e2ee_api_base`, `chutes_chute_ids`,
`chutes_e2ee_discovery_rounds` (default `3`),
`chutes_e2ee_discovery_interval_seconds` (default `0`), and
`session_refresh_seconds` (default `45`). `chutes_chute_ids` maps configured
upstream model ids to concrete Chutes `chute_id` UUIDs; production Chutes
routes should use it instead of catalog name lookup.

When `PRIVATE_AI_GATEWAY_ADMIN_TOKEN` is set, an admin can inspect and replace
the same config file at runtime:

```bash
curl -H "Authorization: Bearer $PRIVATE_AI_GATEWAY_ADMIN_TOKEN" \
  http://127.0.0.1:8086/v1/admin/upstreams

curl -X PUT -H "Authorization: Bearer $PRIVATE_AI_GATEWAY_ADMIN_TOKEN" \
  -H "content-type: application/json" \
  --data-binary @upstreams.json \
  http://127.0.0.1:8086/v1/admin/upstreams
```

The admin view redacts bearer tokens and returns the active `config_digest`.
`PUT` validates the replacement config, writes it to the single configured file,
and swaps the live upstream router. If no admin token is configured, the admin
endpoint returns `404`.

## Dependencies

The dependency list is intentionally small. Each crate is named below
with the reason it is in the tree:

| Crate | Role |
| --- | --- |
| `serde`, `serde_json` | ACI wire types and JCS input. `preserve_order` so existing JSON structure is preserved through round-trips. |
| `dstack-sdk` | dstack KMS key release, `/Info`, and `/GetQuote` over the guest-agent socket or HTTP endpoint. |
| `sha2` | SHA-256 for canonical digests, report data, receipt signing. |
| `ed25519-dalek` | Workload identity / receipt Ed25519 signing. |
| `k256` | secp256k1 ECDSA recoverable signing per ACI §9.4. |
| `curve25519-dalek`, `x25519-dalek` | dstack-vLLM-proxy Ed25519/X25519 E2EE compatibility profile. |
| `aes-gcm`, `hkdf` | ACI E2EE field encryption. |
| `base64`, `chacha20poly1305`, `flate2`, `ml-kem` | Chutes provider E2EE transport compatible with `chutes-e2ee`. |
| `rand`, `rand_core` | Receipt id randomness. |
| `hex` | Hex encoding for public keys, digests, signatures. |
| `rustls-pemfile`, `x509-parser`, `rustls`, `webpki-roots` | Parse mounted TLS leaf certificates, publish attested SPKI digests, and enforce upstream SPKI bindings. |
| `axum`, `tokio`, `tower` | HTTP server. Axum 0.7 + tokio multi-thread runtime. |
| `reqwest` (rustls-tls) | Upstream HTTP client. Rustls avoids a system OpenSSL dependency inside the dstack image. |
| `dcap-qvl` | Pure-Rust Intel DCAP quote verification for ACI upstream reports. |
| `thiserror` | Library error types. |
| `tracing`, `tracing-subscriber` | Structured logging. |
| `prometheus` | Aggregator-owned `/v1/metrics` counters. The service does not expose upstream metrics. |
| `async-trait` | Async trait helpers on `UpstreamBackend`. |

No NVIDIA / nvtrust crates. GPU attestation is a per-upstream concern
and will arrive with the verifier traits.

## Running

```
cargo test                                  # all unit + integration tests
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
printf '[{"name":"local","base_url":"http://127.0.0.1:9000","models":{"local-model":"local-model"}}]\n' \
  >/tmp/private-ai-gateway-upstreams.json
PRIVATE_AI_GATEWAY_DSTACK_ENDPOINT=unix:/tmp/aci-dstack-sock-dev.dstack.sock \
PRIVATE_AI_GATEWAY_REPO_URL=https://github.com/Dstack-TEE/private-ai-gateway.git \
PRIVATE_AI_GATEWAY_REPO_COMMIT=0123456789abcdef0123456789abcdef01234567 \
PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_PATH=/tmp/private-ai-gateway-upstreams.json \
cargo run --release
```

The dev binary listens on `127.0.0.1:8086` by default.

## Phala multi-upstream smoke

Run the local Docker smoke first after changing routing, upstream verification,
receipt hashing, dynamic upstream config, or model metrics:

```bash
scripts/local_multi_upstream_smoke.sh
```

It runs two upstream ACI aggregators plus one router ACI aggregator under local
Docker Compose. All three mount the forwarded dstack socket from
`DSTACK_SOCK` (default `/tmp/aci-dstack-sock-dev.dstack.sock`), so it exercises
real dstack KMS keys and quotes while avoiding a full Phala deployment. The
router starts with an empty config file, receives its upstream routes through
`PUT /v1/admin/upstreams`, then performs the same routing, receipt, and metrics
assertions as the Phala smoke.

Run the slower real Phala smoke when you need to validate the dstack deployment
surface:

```bash
scripts/phala_multi_upstream_smoke.sh
```

It builds and pushes `Dockerfile.smoke` to `ttl.sh`, deploys two mocked
upstream ACI services, fetches each upstream attestation report to derive the
dstack KMS root policy, then deploys one router with a single upstream config
file mounted into the CVM. It asserts:

- `/v1/models` returns only public model ids
- each public model id routes to the expected upstream
- `request.forwarded` hashes the rewritten upstream request body
- `upstream.verified` is recorded as verified with the upstream model id
- metrics record upstream model ids and never public aliases

Artifacts are written to `/tmp/private-ai-gateway-smoke-router` by default. Set
`IMAGE_REF=<existing-image-ref>` to skip the Docker build/push, or
`WORK_DIR=<path>` to keep artifacts elsewhere.

## ACI roadmap

Order matches the design doc §9 phases.

1. **Provider-specific verifier policies** - add Chutes / Tinfoil /
   NEAR AI policy adapters on top of the ACI/DCAP verifier when their
   public attestation surfaces are stable.
2. **Persistent receipt and retained-body store** - the current
   `GET /v1/receipt/{chat_id}/body` implementation is in-memory
   with expiry. Replace it with a regional store for production
   topology.
3. **Multi-region** - GeoDNS + replicated KMS app-id; no protocol
   change.
