# Router-Mode Soundness Review

Reviewer: Codex (router-mode soundness lane only).
Date: 2026-05-18 UTC.
Scope: soundness, privacy, provenance, attested source/config/image/measurement,
downstream model CVM verification, router-to-model transport binding, and
sensitive-data egress through logs / metrics / billing / tools / retries /
errors. Load balancing and cache locality are owned by the second reviewer and
are out of scope here.

## Summary Conclusion

- Tinfoil `inference.tinfoil.sh` confidential model router: **acceptable with
  conditions**. The router source, config-compose, and runtime config update
  flow are reasonably tight; downstream model CVMs are independently verified
  before being added to the serving pool, and the router-to-model channel is
  TLS-pinned to each enclave's attested key. Conditions are documented below
  (runtime-config integrity, debug-bypass gating in measurement, billing /
  telemetry surface).
- NEAR AI `cloud-api.near.ai` gateway: **acceptable only with stronger
  conditions, narrower than Tinfoil**. I found the public gateway source in
  `nearai/cloud-api`; the build and verifier repos have a credible path from
  an attested compose/image digest to source provenance, and downstream model
  CVMs that are routed via `inference_url` providers are SPKI-pinned after TDX
  + GPU + image-hash verification. I did not complete an independent live
  mapping from the production gateway's attested compose/image digest to the
  inspected `cloud-api` commit. Also, the same gateway supports unattested
  external providers (OpenAI / Anthropic / Gemini) within the same model
  namespace, and the public attestation report only proves model-level TEE
  routing when `model_attestations[]` is present for the requested model.
  Response/conversation features can persist content in JSONB unless higher
  layers encrypt or the storage plane is itself accepted as part of the TCB.
  These widen the trust assumption beyond "gateway TEE attests, model TEE
  attests"; the gateway must constrain which models and features it accepts
  in router mode.
- Router attestation alone is **not sufficient** for either provider in the
  general case. The gateway currently pins only the gateway TLS SPKI. That
  binds the network channel to the gateway TEE but does not by itself
  constrain (a) what runtime config the gateway uses to select downstream
  backends, (b) whether the chosen downstream backend is itself a verified
  TEE, or (c) what auxiliary egress (billing, telemetry, MCP tools, DB
  persistence) the gateway performs.

## Repositories and Commits Inspected

| Repo | URL | Commit (HEAD at review) |
| --- | --- | --- |
| Tinfoil confidential router | https://github.com/tinfoilsh/confidential-model-router | `41b2e93e099baf3dd8085066c205f030b280cadc` |
| NEAR AI cloud-api gateway | https://github.com/nearai/cloud-api | `057135fad9e5f656baa94025d831f55391979334` |
| NEAR AI cloud-verifier | https://github.com/nearai/nearai-cloud-verifier | `8b7830e96aa4c0b2b797a2249616da7de6d0087f` |
| NEAR private-ml-sdk (downstream model side) | https://github.com/nearai/private-ml-sdk | `25c25025c556ab2f797eeda3bab433f38a8ffb7a` |

NEAR gateway source search result: the exact gateway implementation appears to
be `nearai/cloud-api`. `nearai/private-ml-sdk` is model-side evidence/supporting
code, not the gateway. The inspected `cloud-api` `main` commit is newer than the
latest production tag seen locally (`prod-20260514-005534` at
`d91edc97a2f8a0fc08438fbd5216133081797e19`); the live attestation-to-source
mapping remains a required follow-up.

Tinfoil's deployed image is pinned in
`/tmp/confidential-model-router/tinfoil-config.yml`:
`ghcr.io/tinfoilsh/confidential-model-router@sha256:893f9a33e3c24bced341413a2a934e9a4f0453a76787616d42631b3181c472ba`
(v0.0.102).

Live `/.well-known/tinfoil-proxy` snapshot taken at 2026-05-18T06:18:51Z
confirms `models[].measurement.registers[2]` (compose hash) and per-enclave
`tls_key_fp` / `hpke_key` for every advertised model. Live `cloud-api.near.ai`
returned `gateway_attestation.signing_address=0x614bc66f...0407dbb` and, with
`model=google/gemma-4-31B-it&include_tls_fingerprint=true`, returned exactly
one model attestation entry including `tls_cert_fingerprint`, `intel_quote`,
`nvidia_payload`, `event_log`, and `compose_manager_attestation`.

## Commands Run

```
ls /tmp/{confidential-model-router,nearai-cloud-api,nearai-cloud-verifier,private-ml-sdk}
git -C /tmp/confidential-model-router pull && git -C /tmp/confidential-model-router log --oneline -5
git -C /tmp/nearai-cloud-api          pull && git -C /tmp/nearai-cloud-api          log --oneline -5
git -C /tmp/nearai-cloud-verifier     pull && git -C /tmp/nearai-cloud-verifier     log --oneline -5

# Tinfoil router structure / code
cat /tmp/confidential-model-router/main.go
cat /tmp/confidential-model-router/manager/{manager,proxy,http_client,file_inputs}.go
cat /tmp/confidential-model-router/billing/events.go
cat /tmp/confidential-model-router/config/config.go
head -120 /tmp/confidential-model-router/toolruntime/{local_debug_disabled,local_debug_enabled,runtime}.go
cat /tmp/confidential-model-router/tinfoil-config.yml
cat /tmp/confidential-model-router/Dockerfile
cat /tmp/confidential-model-router/config.yml

# NEAR cloud-api structure / code
head -200 /tmp/nearai-cloud-api/crates/services/src/attestation/verification.rs
sed -n '820,940p' /tmp/nearai-cloud-api/crates/services/src/attestation/mod.rs
sed -n '1,300p'   /tmp/nearai-cloud-api/crates/services/src/inference_provider_pool/mod.rs
sed -n '1970,2025p' /tmp/nearai-cloud-api/crates/services/src/inference_provider_pool/mod.rs
cat /tmp/nearai-cloud-api/crates/inference_providers/src/spki_verifier.rs
ls /tmp/nearai-cloud-api/crates/inference_providers/src/external/
sed -n '850,880p' /tmp/nearai-cloud-api/crates/api/src/routes/completions.rs
sed -n '1,60p'    /tmp/nearai-cloud-api/crates/database/src/migrations/sql/V0020__add_response_items_and_workspace_scoping.sql
cat /tmp/nearai-cloud-api/Dockerfile  # reproducible-build prologue

# NEAR cloud verifier
cat /tmp/nearai-cloud-verifier/version_verifier.py

# Gateway side
sed -n '740,840p' src/aci/verifier.rs
sed -n '280,380p' scripts/private_ai_provider_verifier.py

# Live probes
curl -s https://inference.tinfoil.sh/.well-known/tinfoil-proxy
curl -s 'https://cloud-api.near.ai/v1/attestation/report?signing_algo=ecdsa'
curl -s 'https://cloud-api.near.ai/v1/attestation/report?signing_algo=ecdsa&include_tls_fingerprint=true&model=google/gemma-4-31B-it'
```

## Public Docs Consulted

Docs were used as supporting context, not as proof where code/evidence was
missing.

- Tinfoil attestation architecture:
  https://docs.tinfoil.sh/verification/attestation-architecture
- Tinfoil predicate / source-measurement model:
  https://docs.tinfoil.sh/verification/predicate
- NEAR AI Cloud private inference overview:
  https://docs.near.ai/cloud/private-inference/
- NEAR AI Cloud TLS verification:
  https://docs.near.ai/cloud/verification/tls/
- NEAR cloud verifier README:
  `/tmp/nearai-cloud-verifier/README.md`
- Local Private AI Gateway router-mode review guide:
  `docs/router-mode-provider-review.md`

## Evidence Table - Tinfoil `inference.tinfoil.sh`

| Claim | Evidence | Source / code reference | Confidence |
| --- | --- | --- | --- |
| Router source is published and the running image digest is reachable from the dstack compose | `containers[0].image` in `tinfoil-config.yml` pins `ghcr.io/tinfoilsh/confidential-model-router@sha256:893f9a...c472ba` (v0.0.102). The compose hash extends RTMR3 and is observable in the router's quote. | `/tmp/confidential-model-router/tinfoil-config.yml`, `/tmp/confidential-model-router/Dockerfile` | High |
| Router measurement is bound to the public Tinfoil sigstore chain | `manager.NewEnclaveManager` builds a `sigstore.NewClient()`, and per-model `updateModelMeasurements` calls `github.FetchLatestTag`, `github.FetchDigest`, `github.FetchAttestationBundle`, then `sigstoreClient.VerifyAttestation` to get `SourceMeasurement`. | `manager/manager.go:426,473-492` | High |
| Downstream model CVM is verified before being added to the serving pool | `addEnclave` fetches `/.well-known/tinfoil-attestation` over HTTPS, calls `remoteAttestation.Verify()`, optionally `VerifyHardware(hwMeasurements, ...)` for TDX-Guest-V2, and rejects with `measurement mismatch for enclave %s` if `verification.Measurement.Equals(model.SourceMeasurement)` fails. | `manager/manager.go:154-213` | High |
| Router-to-model transport is bound to the attested enclave TLS key | The reverse-proxy and the side-channel HTTP client both wrap `tinfoilClient.TLSBoundRoundTripper{ExpectedPublicKey: tlsKeyFP}`. `tlsKeyFP` is the value `attestation.TLSPublicKey(host)` (also checked against the attested `verification.TLSPublicKeyFP` recorded at add time). Mismatches surface as `tinfoilClient.ErrCertMismatch`. | `manager/proxy.go:155-171,200-220`, `manager/http_client.go:14-37,43-62`, `manager/file_inputs.go:179-188`, `manager/manager.go:201-209` | High |
| Tool-runtime MCP servers are themselves attested enclaves on the same trust path | `MCPServerEndpoint` returns the per-model attested base URL `https://<host>/mcp` with a TLS-pinned `http.Client`; `config.yml` lists `websearch` and `code-execution` as their own model entries with their own `repo` and enclave host (`websearch.tinfoil.sh`, `code-execution.tinfoil.sh`). | `manager/http_client.go:64-83`, `/tmp/confidential-model-router/config.yml` | High |
| Production builds do not include the developer-only on-disk session logger | `local_debug_disabled.go` is the default file (`//go:build !toolruntime_debug`) and stubs every devLog method; `local_debug_enabled.go` only compiles under `toolruntime_debug`. The release `Dockerfile` does not set this build tag, so prompt/content disk logging is physically absent in the deployed binary. | `toolruntime/local_debug_disabled.go:1,15-35`, `toolruntime/local_debug_enabled.go:1` | High |
| Billing events do not carry plaintext request or response content | `billing.Event` struct fields are `{Timestamp, UserID, Model, PromptTokens, CompletionTokens, TotalTokens, RequestID, Enclave, RequestPath, Streaming, APIKey}`. The local log line masks APIKey via `maskAPIKey`; the wire `contract.Event` carries the raw APIKey + token counts + `{model, route, streaming, enclave}` attributes, no message text. | `billing/events.go:18-30,38-46,70-114` | High |
| Streaming usage injection is request-shaping, not exfiltration | `ensureStreamingUsageOptions` forces `stream_options.include_usage=true` and `continuous_usage_stats=true`. It also sets the request header `X-Tinfoil-Client-Requested-Usage` only when the client opted in itself, so the gateway can decide whether to filter usage-only chunks before forwarding to the client. No content leaves. | `main.go:194-219,525-530`, `manager/proxy.go:256-340` | High |
| Router does not log prompt or completion content | `grep -nE 'log\..*(prompt|message|content|tool_call|input)'` against `manager/`, `billing/`, and `toolruntime/` finds no matches. Errors log enclave host, model, reason (bounded label from `classifyProxyError`), and a sanitized error string - no body. | repo-wide grep; `manager/proxy.go:51-78,183-200` | Medium-high |
| Debug bypass that connects to a local non-attested MCP is gated on `--debug` flag | `main.go` flag `debug` (env `DEBUG`); `em.SetDebugMode(*debug)` controls `EnclaveManager.debug`; `MCPServerEndpoint` honors `LOCAL_MCP_ENDPOINT_<MODEL>` only when `em.debug` is true and logs `WARN debug mode enabled` at startup. | `main.go:75-80,296-300`, `manager/http_client.go:64-83`, `manager/manager.go:70-77` | High |
| Initial config can be integrity-pinned via `INIT_CONFIG_URL=...@sha256:<hex>` | `config.Load` with `sha256_required=true` errors if the URL lacks `@sha256:` and verifies the fetched bytes' SHA256 against the fragment. | `config/config.go:53-99,101-118` | High |

## Risks / Open Questions - Tinfoil

1. **Runtime config update URL is not integrity-pinned by default.** `main.go:72` defaults `UPDATE_CONFIG_URL` to `https://raw.githubusercontent.com/tinfoilsh/confidential-model-router/main/config.yml`, and `config.Load` for the update flow is called with `sha256_required=false`. The dstack-pinned image makes the *initial embedded* `config.yml` part of the measurement, but the periodic refresh (default 5 minutes) overwrites it with whatever the `main` branch publishes at fetch time. Authentic Tinfoil GitHub branch protection is the only thing preventing a model->enclave swap. Mitigating: even if an attacker rewrote the config, `addEnclave` would still refuse a new enclave whose `verification.Measurement.Equals(model.SourceMeasurement)` fails. So the attacker could only swap to backends that also publish a valid sigstore attestation for the same `model.Repo`, or remove entries. Consequence is therefore availability + steering, not direct plaintext exfiltration. Still: from a soundness standpoint, runtime model->host mapping is not part of the router's attested state.
2. **`X-Forwarded-Host` subdomain-based model routing is sensitive to upstream proxy honesty.** `parseModelFromSubdomain` reads `X-Forwarded-Host`. Outside of the public ingress shim, an attacker who can set this header could potentially confuse model selection. In ACI we forward to the router host directly and do not set `X-Forwarded-Host`, so this is informational rather than a gateway-side bug - but the gateway must continue to refrain from setting it.
3. **`Tinfoil-Enclave` response header exposes which enclave answered.** Not sensitive on its own (the gateway can use it for cross-checking), but if the gateway forwards it to end users verbatim, downstream caches/log sinks see a per-request backend host. The gateway should decide whether to strip it before relaying.
4. **Billing/usage reporter ships per-request HMAC-signed events to `api.tinfoil.sh`.** The events do not carry content, but they do carry raw API keys (`APIKey: apiKey` at `billing/events.go:91`). For Private AI Gateway we own the API key in our own gateway, so the Tinfoil-side key is a per-gateway secret rather than per-user, which is fine. We should still confirm that the gateway never accidentally forwards a user-supplied `Authorization` header to Tinfoil; today the adapter swaps to our credential, but a future change must keep that property.
5. **`/v1/models` proxies to `controlPlaneURL=api.tinfoil.sh` without TLS pinning** (`main.go:386-403`). Catalog only, no user content, but reminds us that not every endpoint is bound to a TEE.
6. **MR_TD / RTMR1 / RTMR2 / RTMR3 of the router itself are not independently checked by our verifier bridge.** `verify_tinfoil` extracts `report_data[:32]` and treats it as the SPKI binding (`scripts/private_ai_provider_verifier.py:309-322`). The Phala "TeeVerifier.verify" call we delegate to does verify the quote chain, but our bridge does not surface or assert the router's compose hash or image digest. To gain confidence that the router code is exactly the v0.0.102 source above, we would need to also surface the compose hash and check it against an allowlist we maintain. Today this is implicit trust in Tinfoil's own publish-and-verify pipeline.
7. **Debug build can be flipped at deploy time.** `--debug` is a runtime flag, not a compile-time choice. The dstack `tinfoil-config.yml` shown above does not pass it, but a redeploy with `DEBUG=1` env would re-mint the compose hash (so RTMR3 would change), which is detectable. The bridge should record and compare the compose hash to catch this - see action item below.

## Evidence Table - NEAR AI `cloud-api.near.ai`

| Claim | Evidence | Source / code reference | Confidence |
| --- | --- | --- | --- |
| Cloud-api build/provenance path exists, but live source mapping was not completed | `Dockerfile` pins base images by digest (`rust:1.92.0-bookworm@sha256:9676...`, `debian:bookworm-slim@sha256:78d2...`), uses Debian snapshot.debian.org with pinned package versions, sets `SOURCE_DATE_EPOCH=0`, and builds with `cargo build --release --locked`. `version_verifier.py` extracts the cloud-api image digest from the attested `app_compose.docker_compose_file` and cross-checks GitHub attestations for `nearai/cloud-api` to recover a source commit. This review inspected that mechanism but did not run it end-to-end against the live gateway report and match it to the inspected commit. | `/tmp/nearai-cloud-api/Dockerfile`, `/tmp/nearai-cloud-verifier/version_verifier.py` | Medium |
| Gateway report-data binds (signing_address, TLS SPKI, nonce) | `report_data[0:32] = SHA256(signing_address_bytes || tls_fingerprint_bytes)` when `include_tls_fingerprint=true`, otherwise `report_data[0:32] = signing_address_bytes padded to 32`. Bytes `[32:64]` are the request nonce. | `crates/services/src/attestation/verification.rs:289-365`, `crates/services/src/attestation/mod.rs:820-829` | High |
| Each downstream model backend is independently TDX+GPU+image-hash verified before any user traffic is forwarded | `PoolBackendVerifier::create_verified_client` builds a Bootstrap-mode rustls client, fetches `/v1/attestation/report?model=...&signing_algo=ecdsa&nonce=...&include_tls_fingerprint=true`, calls `AttestationVerifier::verify_attestation_report` (TDX quote via `dcap_qvl`, TCB-not-Debug check, report-data binding, RTMR3 replay + `os-image-hash` / `compose-hash` extraction, optional `ALLOWED_IMAGE_HASHES` allowlist, GPU NRAS verification), then pins the verified SPKI into both the shared and the per-client `FingerprintState`. The serving reqwest client is configured with that pinned verifier, so any reconnect to a different backend fails handshake. | `crates/services/src/inference_provider_pool/mod.rs:135-253`, `crates/services/src/attestation/verification.rs:152-287,366-498` | High |
| Custom rustls verifier enforces the SPKI pin and a Blocked terminal state on attestation failure | `SpkiFingerprintVerifier::verify_server_cert` first runs WebPKI then matches against `FingerprintState::{Bootstrap,Pinned(set),Blocked}`. `Blocked` short-circuits with a TLS error so no plaintext is sent. | `crates/inference_providers/src/spki_verifier.rs:106-169` | High |
| Cloud-api also fronts non-TEE external providers within the same model namespace | `ProviderConfig::{OpenAiCompatible,Anthropic,Gemini}` plus `create_external_provider`. External providers carry an API key (per-model in DB or from env: `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`) and forward over plain TLS (WebPKI only, no attestation). | `crates/services/src/inference_provider_pool/mod.rs:1975-2021`, `crates/inference_providers/src/external/{openai_compatible.rs,anthropic/,gemini/,backend.rs}` | High |
| The public `/v1/attestation/report` response does NOT flag whether a model is verified-backend vs external | Without a model parameter the response is gateway-only and `model_attestations` is empty. With a model parameter the response contains a `model_attestations` entry only for `inference_url`-style backends; for external providers the response is silent (the gateway has no attestation to attach). A caller that ignores this distinction can be served by an external provider while still believing the request is end-to-end TEE-protected. | Live probe: `model_count=0` for unscoped request; `model_count=1` with `model=google/gemma-4-31B-it`. Source: `crates/api/src/routes/attestation.rs:206-220`, `crates/services/src/attestation/mod.rs:675-930` | High |
| Response/conversation features can persist content as plaintext JSONB | `response_items.item JSONB NOT NULL`, `conversation_id UUID REFERENCES conversations(id)`. No column-level encryption or envelope wrapping was found in the migration/repository path. This proves the source has plaintext-at-rest capability for response/conversation features; it does not prove every chat-completions request through `cloud-api.near.ai` creates such a row. | `crates/database/src/migrations/sql/V0020__add_response_items_and_workspace_scoping.sql:1-22`, `crates/services/src/conversations/models.rs:60-89`, `/tmp/nearai-cloud-api/docker-compose.yml` | Medium-high |
| `RUST_LOG=debug` will log full SSE stream events including model output | `tracing::debug!("Completion stream event: {}", json_data)` runs once per SSE frame. | `crates/api/src/routes/completions.rs:860` | High |
| Observability sink is Datadog/OTLP | `docker-compose.yml` defines a Datadog Agent container with OTLP HTTP/gRPC, log container collection, APM, and `DD_TAGS=service:cloud-api`. Whether this is the production sink as well is a deployment policy claim; the source supports it. | `/tmp/nearai-cloud-api/docker-compose.yml` (datadog-agent service) | Medium |
| Auto-redact (`x-auto-redact`) calls a `openai/privacy-filter` model in the same provider pool | `DEFAULT_PII_MODEL = "openai/privacy-filter"`; provider chosen via the same `InferenceProviderPool` (so it is either a verified TEE backend or an external provider, depending on registry). | `crates/services/src/auto_redact/mod.rs:1-55` | High |
| Release builds reject `DEV` and refuse to fall back to ephemeral keys | `#[cfg(debug_assertions)]` branch enables `DEV` fallback only in debug builds; `#[cfg(not(debug_assertions))]` branch always errors when dstack key derivation fails. Release builds also fatal-log if `DEV` is set in the environment ("SECURITY: DEV environment variable is set in a release build"). | `crates/services/src/attestation/mod.rs:60-150,834-924` | High |
| Backend attestation rejects TDX debug mode | `td_report.td_attributes[0] & 0x01 != 0` causes `TdxVerificationFailed("TDX debug mode is enabled - rejecting")`. | `crates/services/src/attestation/verification.rs:218-223` | High |

## Risks / Open Questions - NEAR AI

1. **External-provider escape hatch.** This is the dominant soundness risk for
   router-mode in NEAR. From the user's vantage, "I sent my prompt to
   `cloud-api.near.ai`" gives them an attested transport channel to the
   gateway TEE and a signing-address signature on the response. It does NOT
   tell them whether the actual inference ran inside a verified TDX+GPU
   backend or whether the gateway routed `model=X` to `api.openai.com` with an
   `OPENAI_API_KEY` from its env. The Private AI Gateway must therefore not
   accept arbitrary `model` strings for NEAR in router mode; it must restrict
   itself to model IDs for which a fresh per-request `/v1/attestation/report?model=...`
   call returns a non-empty `model_attestations[]` whose `tls_cert_fingerprint`
   we then verify and pin. Current gateway behavior (channel binding from
   `gateway_attestation.tls_cert_fingerprint`) closes only the gateway leg.
2. **Response/conversation plaintext at rest.** The source has API paths that
   store response/conversation items as plaintext JSONB in Postgres. I did not
   prove that a plain chat-completions request without conversation state
   creates such a row, so this is a feature-surface risk rather than a proven
   leak for every request. For Private AI Gateway's "end-to-end TEE" framing, any enabled
   persistence path is a privacy reduction unless the storage plane is itself
   attested and operated under the same trust domain as the gateway, AND we
   have evidence of that (e.g. the Postgres image digest appears in the same
   compose attested by the gateway quote, with no external mount). The
   compose-manager attestation (`compose_manager_attestation` from the live
   probe) hints this could be checked, but we have not done so.
3. **Optional `ALLOWED_IMAGE_HASHES`.** `AttestationVerifier::from_env`
   defaults to an empty allowlist, in which case backend OS-image-hash check
   is skipped - any attested-quote backend running an arbitrary OS image
   passes. We assume operators in production set this, but it is not enforced
   by code default and we have no proof it is set on `cloud-api.near.ai` today.
4. **Optional `REQUIRE_TCB_UP_TO_DATE`.** Default off. Out-of-date TCB is
   logged as a warning, not rejected. A TCB regression on a backend would not
   close the channel.
5. **`tracing::debug!` of stream events.** If a deployed cloud-api ever runs
   with `RUST_LOG=debug` (intentionally or transiently), every SSE frame
   carrying model output is emitted to the configured tracing sink, which -
   per `docker-compose.yml` - is Datadog. There is no compile-time gate
   preventing this; only an operational discipline of `RUST_LOG=info`.
6. **External-provider API keys leave the TEE.** When a model is external,
   the gateway forwards prompts with an API key over a non-attested TLS link.
   This is correct for that backend type but obviously voids any TEE privacy
   claim for those models.
7. **Gateway's verifier bridge does not surface the gateway compose / image
   hash.** `scripts/private_ai_provider_verifier.py:327-376` returns only the
   gateway TLS SPKI as `channel_bindings`. We never assert the cloud-api
   image digest from the attested compose. As a result our verification "the
   gateway is the published cloud-api code at commit Z" relies on NEAR's own
   publish chain rather than an independent check.
8. **Multi-instance fingerprint accumulation is by design but widens the pin
   set over time.** `discover_model` and the cumulative discovery loop add
   each newly-observed verified SPKI to the shared `FingerprintState::Pinned`
   set. A backend whose key was once verified remains pinned across refresh
   cycles unless evicted. We did not find an explicit pin-set eviction path
   for retired backends; the failure-counts cleanup runs only on full
   refresh.

## Cross-Cutting Concerns That Apply to Both Routers

1. **Verifying the router/gateway quote is not the same as verifying the
   model.** The two are linked by the gateway proving (via attested source
   code) that it will only forward to attested backends. That linkage is a
   software claim; it can be broken by config substitution (Tinfoil's
   `UPDATE_CONFIG_URL`) or model-routing policy (NEAR's external-provider
   support). The gateway should treat router-mode as "trust this accepted
   router TEE version to verify model TEEs on our behalf", not as "the model
   TEE is directly verified by the gateway".
2. **Plaintext crosses the trust boundary inside the router.** For both
   providers the user's request body is decrypted at the router TLS
   termination and re-encrypted under a different transport when forwarded
   downstream. The router does observe and may serialize/inspect the
   payload (tool-call rewriting, file conversion, auto-redact, billing token
   extraction). Anything that escapes the attested process - DB writes,
   telemetry, billing - is a privacy edge.
3. **No "selected backend" proof returned to the user.** Tinfoil returns
   `Tinfoil-Enclave: <host>` as a header (so the user can match it against
   `/.well-known/tinfoil-proxy`'s model entry - a one-step but provider-
   trusted claim). NEAR's `/v1/attestation/report?model=...` returns the model
   attestation, but no per-request receipt that this exact downstream answered
   *this exact request*. Soundness here depends on the router not lying about
   which backend it used after the fact.
4. **Retry and error paths.** Tinfoil may skip overloaded candidates before
   forwarding, but once it calls `ServeHTTP` for a selected enclave it does
   not re-pick a backend after a send failure or partial response. Every
   candidate still has to pass the same `ShouldReject()` and TLS pin because
   enclaves are added only through `addEnclave`. NEAR's pool can re-verify
   and retry before a request is committed to a backend; it does not downgrade
   to an unverified backend on failure. Both paths are compatible with the
   trust model, while non-idempotent completion retries remain the client's
   responsibility.

## Gateway-Side Status (`private-ai-gateway`)

What we do today (read-only review, no changes made):

- `TinfoilProviderVerifier` / `NearAiProviderVerifier` wrap
  `ExternalProviderVerifier::private_inference(...)` and run our Python bridge
  `scripts/private_ai_provider_verifier.py` (`verify_tinfoil`, `verify_nearai`).
  Each returns one `channel_bindings` entry of type `tls_spki_sha256` bound to
  the provider URL origin (`src/aci/verifier.rs:749-839`,
  `scripts/private_ai_provider_verifier.py:286-376`).
- The ACI backend forwards over an HTTPS connection that enforces the verified
  SPKI; that part of the chain is consistent with what both routers expect.
- We do not pin model-side measurements ourselves, do not surface the
  router/gateway compose-hash, and do not differentiate per-model TEE-backed
  vs external-provider models on NEAR.

## Live and Unit Tests Performed

Live probes (performed during this review, not artifact-archived):

- `GET https://inference.tinfoil.sh/.well-known/tinfoil-proxy` returned the
  current model/enclave map with measurement registers and per-enclave
  `tls_key_fp` and `hpke_key`. Tinfoil's published `tinfoil-config.yml` pins
  the router image to v0.0.102 `@sha256:893f9a...c472ba`.
- `GET https://cloud-api.near.ai/v1/attestation/report?signing_algo=ecdsa`
  returned only `gateway_attestation` with `signing_address=0x614bc66...` and
  no `tls_cert_fingerprint`; `model_attestations=[]`.
- Same endpoint with `&model=google/gemma-4-31B-it&include_tls_fingerprint=true`
  returned `gateway_attestation.tls_cert_fingerprint=aaeaf5a2...` and one
  `model_attestations[]` entry including `intel_quote`, `nvidia_payload`,
  `event_log`, `tls_cert_fingerprint`, `ohttp_attestation`, and
  `compose_manager_attestation`.
- Pre-existing gateway artifacts at
  `/tmp/private-ai-gateway-live-e2e/20260518-053809` (Tinfoil) and
  `/tmp/private-ai-gateway-live-e2e/20260518-053819` (NEAR AI) confirm the receipt
  chain currently accepts both providers in router mode.

Unit-test review:

- Tinfoil router has `manager/manager_test.go`, `manager/proxy_test.go`,
  `manager/http_client_test.go`, `billing/events_test.go`, but none of these
  assert the production absence of `LOCAL_MCP_ENDPOINT_*` bypass or the
  absence of `--debug`. The `slowHeaderTripper` is exercised but the
  TLS-binding error path is exercised only via mocks of
  `tinfoilClient.TLSBoundRoundTripper`.
- NEAR cloud-api has `crates/inference_providers/src/spki_verifier.rs::tests`
  for fingerprint-state transitions and a 1300-line `mock.rs` for provider
  behavior; `crates/services/src/attestation/mod.rs` includes unit coverage
  of report-data verification but not of the external-provider routing path
  in router-mode.

Proposed live tests (not run by this review; should be added to the e2e
suite):

1. **NEAR external-provider sentinel.** Issue a `chat.completions` for a model
   ID that NEAR serves via an external provider (if any are publicly enabled
   under the same `cloud-api.near.ai` URL). Confirm that
   `/v1/attestation/report?model=<that-id>&include_tls_fingerprint=true`
   returns no `model_attestations[]` entry. The gateway must refuse such
   models in router mode. The presence/absence of `model_attestations[]`
   is the only public signal.
2. **Tinfoil enclave-pin honesty.** For a request to `inference.tinfoil.sh`,
   verify the `Tinfoil-Enclave` response header matches one of the enclaves
   advertised at `/.well-known/tinfoil-proxy` for the requested model, and
   that an independent fetch of `/.well-known/tinfoil-attestation` on that
   enclave hostname returns the same `verification.Measurement` as the
   sigstore-verified `SourceMeasurement` for the model's GitHub repo + tag.
3. **Compose-hash pinning for both gateways.** Extend the verifier bridge so
   that on `verify_tinfoil` / `verify_nearai` it also surfaces the gateway's
   RTMR3-derived `compose-hash` (or, for Tinfoil, the router-image digest
   from the attested compose) and checks it against a gateway-side
   allowlist. Today we accept any compose hash that the router publishes
   under its own measurement; that delegates the "is this the audited
   source?" question entirely to the provider.
4. **Tinfoil `LOCAL_MCP_ENDPOINT_*` absence.** From inside a router enclave we
   cannot easily prove the absence of an env var, but on the gateway side
   we can refuse to accept a router whose published image digest is not on a
   known-good list (same allowlist as #3). The `--debug` flag does not flip
   the binary; it does flip behavior at runtime, but turning it on would
   change the compose's `command` arguments and thus the dstack compose hash
   that ends up in RTMR3. A compose-hash allowlist makes this detectable.
5. **NEAR per-stream-frame leakage probe.** Send a request to NEAR with a
   distinctive marker string in the prompt while we monitor any logs the
   cloud-api operator exposes (or via a dedicated test deployment of
   cloud-api with `RUST_LOG=info` confirmed). Confirm no inbound side-channel
   log line contains the marker. We cannot do this against the production
   `cloud-api.near.ai` because we do not see its logs; this is an open
   external dependency.
6. **External provider sanity.** With the per-provider verifier bridge,
   confirm that requests to model IDs we don't accept produce a hard 503 with
   "no verified channel binding" rather than ever forwarding.

## Recommended Changes to the Gateway / Provider Adapter

In priority order. None of these are implementation changes I made - they are
items for the team to schedule, and they should be discussed before any code
change.

1. **(P0, NEAR) Enforce model allowlist for router mode.** The Private AI Gateway
   gateway must reject any model in NEAR router mode for which
   `/v1/attestation/report?model=<id>&include_tls_fingerprint=true` does not
   return at least one `model_attestations[]` entry whose `tls_cert_fingerprint`,
   `intel_quote`, and `nvidia_payload` we can verify. The current
   `gateway_attestation.tls_cert_fingerprint`-only binding is necessary but
   not sufficient: it does not exclude external-provider routing inside the
   gateway. Without this, any user-visible "verified TEE" claim is wrong for
   external-routed models.
2. **(P0, both) Surface gateway/router compose-hash in channel bindings and
   pin to a gateway-side allowlist.** Extend
   `scripts/private_ai_provider_verifier.py:verify_tinfoil` /
   `verify_nearai` to extract the RTMR3 `compose-hash` (NEAR) /
   compose-image-digest (Tinfoil's `tinfoil-config.yml`-pinned image) and
   include it as a second channel binding (or as an
   `attested_source_digest`). Refuse verification when the value is not on
   a gateway-maintained allowlist that we update by reviewing the
   corresponding GitHub source/release. This converts "trust the provider's
   publish pipeline" into "trust this specific version we audited."
3. **(P1, NEAR) Document and externally encode the "no plaintext at rest"
   stance.** Either (a) verify via the cloud-api `compose_manager_attestation`
   that any Postgres/cache instances are inside the same attested compose
   and never receive plaintext message content, or (b) restrict our adapter
   to API calls that do not create a conversation/response record in NEAR's
   DB (so far chat-completions without a conversation handle appears to be
   the safest path; this needs confirmation against the cloud-api
   conversations routes).
4. **(P1, Tinfoil) Optionally strip or rename the `Tinfoil-Enclave` response
   header before relaying.** If we want enclave-host visibility for our own
   logging, retain it on our side as an `X-ACI-Verified-Enclave-Hint`
   instead of forwarding the provider's raw header to end users.
5. **(P2, both) Add a gateway-side regression test that issues a chat
   request and asserts no provider-side stream frame, billing event, or
   error response carries the request marker string back out as a verifiable
   side channel.** We cannot inspect provider logs, but we can detect any
   leak that reaches headers, response bodies, receipts, or our own metrics.
6. **(P2, NEAR) Track upstream `compose_manager_attestation` and image-hash
   policy.** Whenever NEAR enables `ALLOWED_IMAGE_HASHES` or
   `REQUIRE_TCB_UP_TO_DATE`, fold those defaults into our verifier bridge
   policy so we don't silently degrade if NEAR loosens the gateway env.
7. **(P2, both) Review the verifier bridge for retry / error paths.** Today
   `verify_tinfoil` / `verify_nearai` only emit "verified" or "failed".
   Confirm there is no quiet downgrade to a half-verified state under a
   transient failure (e.g. NRAS unreachable for GPU evidence).

## What This Review Did NOT Cover

- Load balancing, queue depth, prompt-cache / KV-cache locality (owned by the
  second reviewer; the relevant code is `manager/circuitbreaker.go`,
  `manager/metrics.go`, `manager/manager.go::NextEnclave` for Tinfoil and
  `inference_provider_pool/mod.rs` + `inference_providers/src/vllm/prefix_router.rs`
  for NEAR).
- Chutes E2EE transport and any direct model-CVM endpoints (out of scope per
  `router-mode-provider-review.md`).
- A deep dive into NEAR's compose-manager and Postgres deployment topology
  beyond what is visible in this repo.
- An end-to-end signature-verification audit of the response signature
  chain emitted by either provider; we relied on the existing
  `examples/verify_aci_artifacts.rs` and `tests/receipt.rs` for that.
