# Private AI Gateway

Private AI Gateway is a Rust implementation of an **Attested Confidential
Inference (ACI)** service. It exposes OpenAI-compatible inference APIs, proves
the gateway workload identity through dstack attestation, verifies configured
upstream providers before forwarding private prompts, and signs receipts that
bind requests, responses, rewrites, and upstream verification facts.

Use it when you need an AI inference gateway whose downstream users can verify
which workload handled their request, which provider route was used, and what
the gateway observed before and after optional middleware logic.

This repository is a developer preview for the ACI draft in
[`Dstack-TEE/dstack#694`](https://github.com/Dstack-TEE/dstack/pull/694). It is
also the workload that
[`git-launcher`](https://github.com/Dstack-TEE/dstack-examples/tree/main/git-launcher)
can fetch, build, and run inside a dstack v2 application VM.

## What It Does

- Serves OpenAI-compatible `/v1/chat/completions`, `/v1/completions`,
  `/v1/embeddings`, and `/v1/models`.
- Publishes `/v1/attestation/report` for the gateway workload identity and
  keyset.
- Issues signed ACI receipts through `/v1/receipt/{chat_id}` and the legacy
  `/v1/signature/{chat_id}` alias.
- Supports downstream ACI E2EE and vLLM-proxy-compatible legacy E2EE profiles.
- Verifies upstream providers before forwarding when verification is required.
- Supports Tinfoil, NEAR AI, Chutes, ACI/DCAP upstreams, and generic
  OpenAI-compatible upstreams with explicit TLS binding.
- Runs with or without a plaintext middleware slot for auth, billing, policy,
  cache-aware routing, model catalog shaping, request rewrites, and response
  post-processing.

## Status

`0.1.0` is a developer preview. The request path is implemented, but production
release still depends on provider strict-release review, durable operational
storage decisions, and production compose wiring for a concrete middleware
container.

| Area | Status |
| --- | --- |
| Workload identity, keyset digest, attestation report | Implemented |
| Signed receipts and transparency event log | Implemented |
| Chat/completions, streaming, embeddings, `/v1/models` | Implemented; embeddings are buffered |
| Downstream ACI E2EE and legacy vLLM E2EE | Implemented for chat/completions/embeddings; streaming E2EE for chat/completions |
| Runtime upstream config file and admin API | Implemented |
| Gateway-owned Prometheus metrics | Implemented |
| Provider adapters | Implemented for Tinfoil, NEAR AI, Chutes, ACI/DCAP, and OpenAI-compatible TLS-bound upstreams |
| Middleware framework | Implemented over HTTP on Unix domain sockets |
| Receipt/body store | In-memory; receipt TTL is configurable, body retention defaults to disabled |
| Public transparency log | Not implemented |

The binary has no ephemeral-key or stub-quote startup mode. It loads identity,
receipt-signing, and E2EE keys from dstack KMS through the Rust `dstack-sdk`,
and it uses the same SDK for TDX quotes.

## Architecture

```text
downstream user / OpenAI SDK
  -> ACI frontend
  -> optional middleware over UDS
  -> verified-provider backend
  -> upstream provider
```

The frontend owns downstream ACI behavior: request parsing, downstream E2EE
termination, user-facing model names, response E2EE, and receipt finalization.

The backend owns provider trust: configured target validation, model rewrite,
upstream verification leases, upstream TLS or E2EE channel binding, provider
request forwarding, and backend-authored receipt facts.

Middleware is optional. It sees plaintext after downstream E2EE termination and
may implement business logic, but it does not own ACI signing or provider
verification. Middleware chooses a configured target route and calls the
internal backend over a Unix domain socket. The frontend signs the receipt only
after middleware returns the final user-visible response.

See:

- [Frontend, Middleware, Backend Design](docs/frontend-middleware-backend.md)
- [Middleware Integration Guide](docs/middleware-integration.md)
- [Upstream Verification and Lease Lifecycle](docs/upstream-verification-lifecycle.md)

## Quick Start For Local Development

Prerequisites:

- Rust stable toolchain.
- A reachable dstack SDK endpoint. By default the gateway uses
  `/var/run/dstack.sock`; for local development you can point
  `PRIVATE_AI_GATEWAY_DSTACK_ENDPOINT` at a forwarded dstack socket.
- An upstream config file. An empty file is valid, but inference routes require
  at least one configured upstream.

Run checks:

```bash
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

Run a local gateway:

```bash
cat >/tmp/private-ai-gateway-upstreams.json <<'JSON'
[
  {
    "name": "local",
    "provider": "openai-compatible",
    "base_url": "https://example-upstream.invalid",
    "models": {
      "local-model": "upstream-model"
    },
    "tls_spki_sha256": ["<64-hex-spki-sha256>"]
  }
]
JSON

PRIVATE_AI_GATEWAY_DSTACK_ENDPOINT=unix:/tmp/aci-dstack-sock-dev.dstack.sock \
PRIVATE_AI_GATEWAY_REPO_URL=https://github.com/Dstack-TEE/private-ai-gateway.git \
PRIVATE_AI_GATEWAY_REPO_COMMIT=0123456789abcdef0123456789abcdef01234567 \
PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_PATH=/tmp/private-ai-gateway-upstreams.json \
cargo run --release --bin private-ai-gateway
```

The gateway listens on `127.0.0.1:8086` by default.

Check the identity surface:

```bash
curl -sS http://127.0.0.1:8086/
curl -sS 'http://127.0.0.1:8086/v1/attestation/report?nonce=test'
```

Send an OpenAI-compatible request:

```bash
curl -sS http://127.0.0.1:8086/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{
    "model": "local-model",
    "messages": [{"role": "user", "content": "Say hello in one sentence."}]
  }'
```

For a complete local multi-upstream smoke test that exercises real dstack KMS
keys and quotes through a forwarded socket, run:

```bash
scripts/local_multi_upstream_smoke.sh
```

## Deploy With Git Launcher

The recommended dstack deployment path uses `git-launcher`:

1. `git-launcher` clones this repo at a pinned commit.
2. It runs this repo's `entrypoint.sh`.
3. `entrypoint.sh` builds `private-ai-gateway` with `cargo build --release
   --locked --bin private-ai-gateway`.
4. The built binary runs with runtime config from Compose environment, mounted
   files, dstack encrypted secrets, and dstack KMS.

The launcher stays generic. Build, install, and run logic belongs to this repo.
For production, prefer a Rust-capable gateway image so the toolchain is covered
by a gateway-owned image digest instead of installing Rust at boot.

Deployment files:

- [deploy/README.md](deploy/README.md)
- [deploy/compose.yaml](deploy/compose.yaml)
- [deploy/upstreams.example.json](deploy/upstreams.example.json)
- [entrypoint.sh](entrypoint.sh)

## Configure Upstreams

The gateway has one mutable upstream config file. Set
`PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_PATH`; if unset, the default is
`/var/lib/private-ai-gateway/upstreams.json`.

A missing, empty, or whitespace-only file is valid and means no upstreams are
configured yet. The config is a JSON array:

```json
[
  {
    "name": "tinfoil-glm51",
    "provider": "tinfoil",
    "base_url": "https://inference.tinfoil.sh",
    "models": {
      "glm51-tinfoil": "glm-5-1"
    },
    "bearer_token": "<tinfoil-api-key>"
  }
]
```

`models` maps public model ids to provider-facing upstream model ids. In
no-middleware mode, the public model id is also the target route id. In
middleware mode, middleware selects a backend target route of this form:

```text
<upstream name>:<public model id in upstream config>
```

Supported `provider` values:

| Provider | Use |
| --- | --- |
| `openai-compatible` | Generic OpenAI-compatible upstream with configured TLS SPKI or certificate binding. |
| `aci-dcap` | Upstream ACI service that exposes ACI attestation and dstack/DCAP evidence. |
| `tinfoil` | Tinfoil provider adapter using provider-owned verification through `private-ai-verifier`. |
| `near-ai` | NEAR AI gateway adapter with TLS binding from the provider report. |
| `chutes` | Chutes adapter with provider E2EE key verification and encrypted `/e2e/invoke` transport. |

For one-command Compose deployments, set
`PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_SEED_PATH` to a read-only seed file. The
gateway copies the seed only when the mutable config path is missing or empty.
An existing admin-updated config is never overwritten.

When `PRIVATE_AI_GATEWAY_ADMIN_TOKEN` is set, operators can inspect and replace
the live config:

```bash
curl -H "Authorization: Bearer $PRIVATE_AI_GATEWAY_ADMIN_TOKEN" \
  http://127.0.0.1:8086/v1/admin/upstreams

curl -X PUT \
  -H "Authorization: Bearer $PRIVATE_AI_GATEWAY_ADMIN_TOKEN" \
  -H "content-type: application/json" \
  --data-binary @upstreams.json \
  http://127.0.0.1:8086/v1/admin/upstreams
```

The admin view redacts bearer tokens and returns the active `config_digest`.
If no admin token is configured, the admin endpoint returns `404`.

## Enable Middleware

Middleware mode is enabled by a middleware Unix socket path. The gateway also
starts an internal backend socket for middleware to call:

```bash
PRIVATE_AI_GATEWAY_MIDDLEWARE_UDS_PATH=/run/private-ai-gateway/middleware.sock
PRIVATE_AI_GATEWAY_BACKEND_UDS_PATH=/run/private-ai-gateway/backend.sock
```

In middleware mode:

- Public `/v1/models` is forwarded to middleware.
- Public inference requests are decrypted and normalized by the
  frontend, then forwarded to middleware as plaintext HTTP over UDS.
- User headers, including `Authorization`, are forwarded to middleware for
  middleware-owned auth and routing. Gateway-owned and stale E2EE protocol
  headers are stripped.
- Middleware calls `POST /internal/forward` with a one-use request id and a
  configured target route.
- Streaming responses stay streaming across backend, middleware, and frontend.
- Middleware-generated OpenAI-compatible responses are passed through downstream
  E2EE when the original user request used E2EE.

Read [docs/middleware-integration.md](docs/middleware-integration.md) before
writing middleware.

## API Surface

| Endpoint | Purpose |
| --- | --- |
| `GET /` | Basic ACI version, workload id, and keyset digest. |
| `GET /v1/models` | OpenAI-compatible model list from backend or middleware. |
| `POST /v1/chat/completions` | OpenAI-compatible chat completions. |
| `POST /v1/completions` | OpenAI-compatible legacy completions. |
| `POST /v1/embeddings` | OpenAI-compatible buffered embeddings. |
| `GET /v1/attestation/report?nonce=<n>` | Gateway workload identity and keyset evidence. |
| `GET /v1/receipt/{chat_id}` | Signed ACI receipt by chat id. |
| `GET /v1/signature/{chat_id}` | Legacy alias of the receipt endpoint. |
| `GET /v1/receipt/{chat_id}/body` | Retained provider-facing request body when retention is enabled. |
| `GET /v1/metrics` | Gateway-owned Prometheus metrics. |
| `GET /v1/admin/upstreams` | Authenticated upstream config snapshot. |
| `PUT /v1/admin/upstreams` | Authenticated upstream config replacement. |

## Trust Model

The downstream relying party verifies the gateway first, then uses the verified
gateway identity to evaluate responses and receipts.

1. `GET /v1/attestation/report` proves the gateway workload identity, keyset,
   source provenance, and optional client-facing TLS SPKI binding.
2. The gateway keyset endorses receipt signing keys and E2EE keys.
3. Each request receipt records the frontend-observed request, middleware route
   selection when present, provider-facing request, upstream verification event,
   provider response hash, final returned response hash, and any request or
   response modification events.
4. Upstream verification is fail-closed by default. If a request requires
   upstream verification and no verified enforceable binding exists, the
   gateway does not forward the prompt.

Provider-specific verification stays inside provider adapters. The upstream
config selects a provider and model map; it does not expose arbitrary verifier
commands or policy DSLs.

## Runtime Configuration

Use `PRIVATE_AI_GATEWAY_*` variables. Older `DSTACK_LLM_ROUTER_*` aliases are
still accepted for compatibility; the `PRIVATE_AI_GATEWAY_*` value wins when
both are set.

| Setting | Variable | Default |
| --- | --- | --- |
| Public bind address | `PRIVATE_AI_GATEWAY_BIND` | `127.0.0.1:8086` |
| Upstream config path | `PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_PATH` | `/var/lib/private-ai-gateway/upstreams.json` |
| Initial upstream config seed | `PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_SEED_PATH` | unset |
| Admin bearer token | `PRIVATE_AI_GATEWAY_ADMIN_TOKEN` | unset; admin API returns `404` |
| Source-provenance repo URL | `PRIVATE_AI_GATEWAY_REPO_URL` | required |
| Source-provenance commit | `PRIVATE_AI_GATEWAY_REPO_COMMIT` | required |
| Body retention seconds | `PRIVATE_AI_GATEWAY_BODY_RETENTION_SECONDS` | `0` |
| Receipt TTL seconds | `PRIVATE_AI_GATEWAY_RECEIPT_TTL_SECONDS` | `3600` |
| TLS certificate paths | `PRIVATE_AI_GATEWAY_TLS_CERT_PATHS` | unset |
| TLS SPKI SHA-256 list | `PRIVATE_AI_GATEWAY_TLS_SPKI_SHA256` | unset |
| Upstream verifier mode | `PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER` | `none` |
| Upstream verifier cache seconds | `PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER_CACHE_SECONDS` | `300` |
| Upstream connect timeout seconds | `PRIVATE_AI_GATEWAY_UPSTREAM_CONNECT_TIMEOUT_SECONDS` | `10` |
| Upstream read idle timeout seconds | `PRIVATE_AI_GATEWAY_UPSTREAM_READ_TIMEOUT_SECONDS` | `600` |
| Upstream verifier request timeout seconds | `PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER_REQUEST_TIMEOUT_SECONDS` | `60` |
| dstack SDK endpoint | `PRIVATE_AI_GATEWAY_DSTACK_ENDPOINT` | dstack SDK default socket |
| Middleware UDS path | `PRIVATE_AI_GATEWAY_MIDDLEWARE_UDS_PATH` | unset; middleware disabled |
| Internal backend UDS path | `PRIVATE_AI_GATEWAY_BACKEND_UDS_PATH` | `/run/private-ai-gateway/backend.sock` |

Prefer `PRIVATE_AI_GATEWAY_TLS_CERT_PATHS` for client-facing TLS binding. The
gateway reads the mounted leaf certificate, computes `sha256(SPKI)`, and
publishes that digest in the attested keyset. Use
`PRIVATE_AI_GATEWAY_TLS_SPKI_SHA256` only for manual or test deployments. Set
only one of the two.

`PRIVATE_AI_GATEWAY_DSTACK_ENDPOINT` accepts HTTP(S) endpoints and Unix socket
endpoints such as `unix:/var/run/dstack.sock`.

## Test And Smoke Suites

Run the standard local checks:

```bash
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

Run local multi-upstream smoke after changing routing, upstream verification,
receipt hashing, dynamic upstream config, or metrics:

```bash
scripts/local_multi_upstream_smoke.sh
```

Run the slower Phala deployment smoke when you need to validate the deployment
surface:

```bash
scripts/phala_multi_upstream_smoke.sh
```

The Phala smoke deploys two mocked upstream ACI services and one gateway CVM,
then asserts model routing, provider-facing request hashes, verified upstream
events, and metrics model ids.

## Repository Map

```text
src/main.rs                    binary entrypoint and runtime config
src/dstack.rs                  dstack SDK KMS key provider and quote provider
src/aci/                       ACI wire types, canonical JSON, keys, receipts, upstreams
src/aggregator/service.rs      report, forwarding, E2EE, receipt finalization
src/aggregator/upstream_config.rs runtime upstream config and provider adapters
src/http/app.rs                Axum HTTP routers and middleware/backend wiring
docs/                          design notes, provider reviews, middleware guide
deploy/                        git-launcher and dstack compose examples
scripts/                       local and Phala smoke tests
tests/                         unit and integration coverage
```

## More Docs

- [Deployment guide](deploy/README.md)
- [Middleware integration guide](docs/middleware-integration.md)
- [Frontend/middleware/backend architecture](docs/frontend-middleware-backend.md)
- [Live E2E test suite](docs/live-e2e-test-suite.md)
- [Provider audit criteria](docs/reviews/providers/audit-criteria.md)
- [Roadmap](docs/roadmap.md)
