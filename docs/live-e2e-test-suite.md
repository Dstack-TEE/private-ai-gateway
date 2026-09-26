# Run the Live End-to-End Suite

The live suite starts a local gateway, calls real provider APIs, verifies
returned artifacts, and preserves local artifacts for diagnosis. It is an
operator test, not part of the credential-free CI suite. Configured bearer
tokens are redacted, but request and response bytes remain in the artifacts.

The implementation lives in `scripts/live_e2e/`.

> [!WARNING]
> The lifecycle and embeddings cases fail against this gateway. They run
> `cargo run --bin aci`, a binary the repository no longer builds (the CLI is
> `pap`, in `apps/desktop/cli/`), and they assert a
> `transparency.request_modified` receipt event the gateway no longer emits.
> Use preflight, the provider-verifier phase, and the auxiliary scripts for
> diagnosis. A full-matrix failure at those steps is not a gateway regression.

## Design rules

- Never trust unsigned provider metadata. A strict run compares the verifier
  result with reviewed references in `provider_refs/`, not with a live
  endpoint the provider does not sign.
- Never hide provider bugs with post-processing. The cases assert the
  provider's real output. A per-provider parameter such as
  `structured_output_max_tokens` changes the request, never the response.
- Skip rather than fail on unsupported capabilities. A case runs only for
  entries whose `capabilities` list includes it, and the structured-output
  case records `skipped` otherwise.

## What the main runner covers

`scripts/live_e2e/run.py` performs these phases in order:

1. Check tools, credentials, the dstack socket, the gateway port, and optionally the Rust build.
2. Run the provider-verifier bridge for every selected provider.
3. Start a temporary gateway with generated static and upstream configuration.
4. Run one chat lifecycle case for each provider with the `chat` capability.
5. Run one embeddings case for each provider with the `embeddings` capability.
6. Run structured-output fidelity cases for `full` and `strict-release` profiles.
7. Write `summary.json` and stop the gateway.

### Lifecycle case

For each entry with the `chat` capability, the case:

1. Sends a chat completion and requires a 2xx response with an `id` and an
   `x-receipt-id` header.
2. Fetches `/v1/aci/attestation` with a fresh nonce and the receipt from
   `/v1/aci/receipts/{receipt_id}`.
3. Spot-checks the legacy `/v1/signature/{chat_id}` wrapper for its
   `text`, `signature`, `signing_address`, and `signing_algo` fields.
4. Audits the report, receipt, nonce, and exact request and response bytes.
   The audit checks that:
   - the receipt signature verifies under the attested keyset;
   - `request.received.body_hash` equals the exact client body;
   - a request the gateway rewrote shows as a `request.forwarded.body_hash`
     that differs from `request.received.body_hash`;
   - `upstream.verified` has `result: verified` and `required == true`; and
   - the response hash equals the exact response body.
5. Requires the `upstream.verified` event to carry the entry's `binding` type.
6. Fetches each cited session, recomputes its ID from the fetched bytes, checks
   the evidence digest, and requires `tee_attested` to be asserted.

### Embeddings case

For each entry with the `embeddings` capability, the case sends a fixed
`POST /v1/embeddings` request and checks the OpenAI-compatible shape: a
non-empty `data[]` whose first `embedding` is numeric and not all zero.
Embeddings responses carry no `id`, so the case looks up the receipt with the
`x-receipt-id` response header. It then runs the same audit, requires
`receipt.endpoint == "/v1/embeddings"`, and checks the `upstream.verified`
binding and cited session as the lifecycle case does.

The suite does not run load tests, availability measurements, browser verification, or every auxiliary smoke script in `scripts/live_e2e/`.

## Run the local multi-upstream smoke test

The credential-free local smoke suite starts mock ACI upstreams and a gateway with Docker Compose. It requires the forwarded dstack socket but does not call the live provider matrix.

```sh
DSTACK_SOCK=/tmp/aci-dstack-sock-dev.dstack.sock \
  scripts/local_multi_upstream_smoke.sh
```

The script checks direct model routing, TLS-bound ACI-service verification, chat and embeddings receipts, attested sessions, runtime config replacement, and metrics. It tears down the Compose stack on exit unless `KEEP_STACK=1` is set.

Use this suite after changes to the ACI-service adapter, routing, receipts, sessions, config replacement, or the deployment-facing HTTP surface.

## Run the Phala multi-upstream smoke test

`scripts/phala_multi_upstream_smoke.sh` runs the same checks on Phala Cloud. It builds and pushes a smoke image (or deploys `IMAGE_REF`), deploys two mock ACI upstreams and one router CVM with a mounted upstream config, and checks routing, receipts, and metrics.

```sh
scripts/phala_multi_upstream_smoke.sh --help
scripts/phala_multi_upstream_smoke.sh
```

It needs `docker buildx`, the `phala` CLI, `curl`, `jq`, `cargo`, `sha256sum`, and `awk`, and writes compose files, reports, receipts, and metrics under `WORK_DIR` (default `/tmp/private-ai-gateway-smoke-router`). The script does not delete the CVMs it deploys.

## Prerequisites

Install the project dependencies and build tools:

```sh
uv sync --locked
cargo build --bin private-ai-gateway
```

The main provider matrix requires the credentials named in `scripts/live_e2e/providers.json`:

```sh
export TINFOIL_API_KEY='...'
export NEARAI_API_KEY='...'
export CHUTES_API_KEY='...'
```

The runner loads a dotenv file before reading the environment. Its default is `.env` in the parent directory of this repository, not `.env` inside the repository. Override it explicitly when needed:

```sh
uv run python scripts/live_e2e/run.py --env-file .env
```

Do not commit provider credentials or generated upstream configuration.

### dstack socket

The temporary gateway needs a dstack key provider. By default the suite expects:

```text
unix:/tmp/aci-dstack-sock-dev.dstack.sock
```

Start a local dstack simulator or forward a trusted test CVM socket to that path before running the suite. Use `--dstack-endpoint` to select another endpoint.

Preflight and the gateway launcher set `DSTACK_VERIFIER_URL` to `http://localhost:18080` when it is not already set, so the NEAR AI and Phala direct verifiers use that dstack verifier during the run. NEAR AI entries declare this variable as a prerequisite.

The repository contains a vendored `scripts/confidential_verifier` package. Set `PRIVATE_AI_VERIFIER_DIR` only when deliberately testing another checkout.

## Run a preflight check

Preflight catches missing credentials, missing executables, a missing Unix socket, a busy port, and a failed gateway build without sending inference requests:

```sh
uv run python scripts/live_e2e/preflight.py \
  --env-file .env \
  --port 18086
```

Use `--no-build` only when the binary was already built and the goal is to avoid another compilation pass.

## Run the suite

Run the default quick profile against every configured provider:

```sh
uv run python scripts/live_e2e/run.py \
  --env-file .env \
  --profile quick
```

The runner writes its terminal result to `summary.json` in the artifact
directory printed at startup. Treat the run as successful only when the
selected cases are marked passed and the process exits with status 0, subject
to the warning at the top of this page.

Select one or more entries with repeated `--provider` arguments. A selector can match the entry name, provider type, or public model alias:

```sh
uv run python scripts/live_e2e/run.py \
  --env-file .env \
  --provider tinfoil-live \
  --provider chutes-live
```

Pass `--port 0` to allocate a free local port automatically.

## Profiles

| Profile | Provider verification | Chat and embeddings | Structured outputs |
| --- | --- | --- | --- |
| `quick` | Live verifier result and channel-binding checks | Yes | No |
| `full` | Same as `quick` | Yes | Yes, for entries with the capability |
| `strict-release` | Also checks the configured model and expected binding against `provider_refs/<provider>.json` | Yes | Yes, for entries with the capability |

Strict references are allowlists, not recorded golden responses. The strict check validates that the selected model is accepted and that the verifier emitted the expected binding type. It does not pin every claim, measurement, or evidence byte.

`--skip-provider-verify` skips the standalone bridge phase. Gateway requests still use the verifier configured for their provider, so this option does not turn constrained inference into unverified forwarding.

## Provider matrix format

The providers file is a JSON array. Required fields are:

| Field | Meaning |
| --- | --- |
| `name` | Unique test entry and generated upstream name. |
| `provider` | Gateway provider type. |
| `base_url` | Provider HTTPS origin. |
| `public_model` | Model alias exposed by the temporary gateway. |
| `upstream_model` | Provider model identifier. |
| `api_key_env` | Environment variable containing the provider credential. |
| `binding` | Channel-binding type the verifier must return. |

Optional fields are:

| Field | Meaning |
| --- | --- |
| `capabilities` | Cases to enable, including `chat`, `embeddings`, and `structured_outputs`. Other labels document provider features for auxiliary tests. |
| `requires` | Additional environment variables preflight must find. |
| `structured_output_max_tokens` | Token limit for the structured-output case; default `512`. Reasoning models need more: Tinfoil `kimi-k2-6` writes a long `message.reasoning` before `message.content`, and at 512 tokens it can stop before any content. |
| `verification_refresh_seconds` | Per-upstream verifier refresh interval. |
| `session_refresh_seconds` | Provider session refresh interval. |
| `chutes_e2ee_api_base` | Alternate Chutes E2EE discovery origin. |
| `chutes_chute_ids` | Map from upstream model to known chute identifier. |
| `chutes_e2ee_discovery_rounds` | Number of Chutes discovery passes. |
| `chutes_e2ee_discovery_interval_seconds` | Delay between Chutes discovery passes. |

Example:

```json
[
  {
    "name": "provider-live",
    "provider": "tinfoil",
    "base_url": "https://inference.example",
    "public_model": "live-model",
    "upstream_model": "provider/model-id",
    "api_key_env": "PROVIDER_API_KEY",
    "binding": "tls_spki_sha256",
    "capabilities": ["chat", "streaming", "structured_outputs"],
    "structured_output_max_tokens": 1024
  }
]
```

Validate a new entry with the quick profile before adding a strict reference. A strict reference should come from a reviewed provider policy, not from copying the first observed value.

## Artifacts

The default artifact root is:

```text
/tmp/private-ai-gateway-live-e2e/<UTC-like local timestamp>/
```

Choose another root with `--artifacts-dir`. Each run includes:

- `summary.json`, including the selected profile and phase results;
- `aggregator.log`;
- `aggregator-upstreams.redacted.json`;
- standalone provider-verifier requests with credentials redacted;
- provider-verifier outputs;
- exact request, response, report, and receipt bytes for each lifecycle case;
- user-verification summaries;
- fetched session records and compact summaries;
- structured-output inputs, outputs, and summaries when enabled.

The runner removes its generated gateway config and state directory on exit. Set `KEEP_LIVE_E2E=1` to retain the temporary directory for debugging. Artifact files can still contain model inputs, outputs, attestation evidence, endpoints, and public identity material. Handle them as test records, even though configured bearer tokens are redacted.

## Auxiliary scripts

The main runner does not dispatch every script in the directory. Run these separately when their narrower behavior is under test:

| Script | Purpose |
| --- | --- |
| `streaming_smoke.py` | Streaming response and receipt smoke test. |
| `chutes_session_smoke.py` | Chutes session discovery and request behavior. |
| `chutes_rate_probe.py` | Chutes rate and capacity observations. |
| `router_refresh_smoke.py` | Boots one router upstream (`near-ai` or `tinfoil`) and checks that background refresh re-verifies its session with no inference traffic. |
| `router_session_smoke.py` | Sends two models through one router upstream and checks that both receipts cite the same session. |
| `bfcl_v4.py` | BFCL tool-calling evaluation. See [Run the BFCL evaluation](#run-the-bfcl-evaluation). |
| `user_verify.py` | Verifies a received response. See [Verify a received response](#verify-a-received-response). |

Read each script's `--help` output before use. These tools call live systems and can consume provider quota. Run the router smokes from `scripts/`, for example `uv run python live_e2e/router_session_smoke.py tinfoil`.

## Verify a received response

`scripts/live_e2e/user_verify.py` answers "I received this response; did it come from the verified gateway and a verified upstream?" It fetches the gateway report with a fresh nonce, the receipt, and the session the receipt's `upstream.verified` event cites, then runs `audit` on them:

```sh
uv run python scripts/live_e2e/user_verify.py \
  --aci-bin pap \
  --base-url https://gateway.example \
  --chat-id <chat-id-or-receipt-id> \
  --bearer-token "$TOKEN" \
  --request-body request.json \
  --response-body response.json
```

`--chat-id` accepts either the response's chat ID or the `x-receipt-id` value. `--request-body` and `--response-body` are optional; with them, the audit also checks the request and response hashes. To audit saved artifacts offline, pass `--report-file` and `--receipt-file`, plus `--session-file` and `--nonce` when available. `--aci-bin` defaults to `aci` or `ACI_BIN`.

The output is the `pap audit --json` transcript: a `checks` array with each check's ID, spec section, status, and detail, and a `verdict` with the pass, fail, and skip counts. The exit status is non-zero unless the verdict is `VERIFIED`.

## Run the BFCL evaluation

`scripts/live_e2e/bfcl_v4.py` runs the Berkeley Function Calling Leaderboard v4 through a local gateway over Chat Completions. It measures native tool-call and multi-turn fidelity; `run.py` stays the source of truth for report, receipt, and binding verification.

```sh
uv run python scripts/live_e2e/bfcl_v4.py \
  --providers-file scripts/live_e2e/providers.glm51.json \
  --test-category single_turn,multi_turn \
  --max-cases 20 \
  --no-build
```

The wrapper needs a BFCL checkout at `--bfcl-dir` (default `BFCL_DIR`, or `../gorilla-bfcl/berkeley-function-call-leaderboard`). It exposes each provider under a BFCL-supported OpenAI Chat Completions model key (`--bfcl-model`), then runs BFCL's own `generate` and `evaluate` commands.

| Flag | Meaning |
| --- | --- |
| `--test-category` | BFCL category or group, repeated or comma-separated. Default `single_turn,multi_turn`. |
| `--sample-rate` | Fraction of cases per category. Default `0.01`. |
| `--sample-mode` | `deterministic` (default) spreads samples by case ID; `random` uses `--sample-seed`. |
| `--min-per-category` | Minimum cases per category. Default `2`, because BFCL's leaderboard step needs two latency points. |
| `--max-cases` | Cap on total cases, keeping a spread of categories. |
| `--provider`, `--port`, `--dstack-endpoint`, `--artifacts-dir`, `--env-file`, `--no-build` | Same meaning as for `run.py`. |

`scripts/live_e2e/providers.glm51.json` maps the GLM 5.1 family across Tinfoil, NEAR AI, and Chutes, so one BFCL run compares three providers on the same model family, each through its own attestation and transport path.

## Failure diagnosis

Start with `summary.json`, then inspect `aggregator.log` and the failing provider directory.

| Failure | First checks |
| --- | --- |
| Missing environment variable | Confirm `--env-file`, the selected matrix entry, and `api_key_env` or `requires`. |
| Missing dstack socket | Start the simulator or pass the correct `--dstack-endpoint`. |
| Provider verification failure | Inspect `provider-verifier-output.json`, then compare it with the provider policy in `docs/providers/`. |
| Binding-type mismatch | Check the matrix `binding` and the verifier's `channel_bindings`. Do not weaken the expectation without reviewing the provider protocol. |
| Gateway never becomes ready | Inspect `aggregator.log`, generated model aliases, and port ownership. |
| Receipt or session assertion failure | Compare the raw receipt, session artifact, and the [API reference](api-reference.md) before changing the test. |
| Strict-reference failure | Confirm the model and policy really changed before updating `provider_refs/`. |

For credential-free validation, use the project checks in [Contributing](../CONTRIBUTING.md) instead of the live suite.
