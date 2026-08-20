# Run the Live End-to-End Suite

The live suite starts a local gateway, calls real provider APIs, verifies returned artifacts, and preserves a redacted artifact bundle for diagnosis. It is an operator test, not part of the credential-free CI suite.

The implementation lives in `scripts/live_e2e/`. This page documents the code that exists today.

## What the main runner covers

`scripts/live_e2e/run.py` performs these phases in order:

1. Check tools, credentials, the dstack socket, the gateway port, and optionally the Rust build.
2. Run the provider-verifier bridge for every selected provider.
3. Start a temporary gateway with generated static and upstream configuration.
4. Run one chat lifecycle case for each provider with the `chat` capability.
5. Run one embeddings case for each provider with the `embeddings` capability.
6. Run structured-output fidelity cases for `full` and `strict-release` profiles.
7. Write `summary.json` and stop the gateway.

The lifecycle and embeddings cases are intended to check inference, receipt
retrieval, artifact verification, the `upstream.verified` event, and the cited
attested-session record. Their request-modification assertions still expect the
removed `transparency.request_modified` event and must be migrated before the
full live matrix is considered compatible with the simplified ACI receipt
schema. They also exercise the legacy report and receipt-wrapper routes as
explicit compatibility surfaces while checking canonical ACI session artifacts.

The suite does not run load tests, availability measurements, browser verification, or every auxiliary smoke script in `scripts/live_e2e/`.

## Run the local multi-upstream smoke test

The credential-free local smoke suite starts mock ACI upstreams and a gateway with Docker Compose. It requires the forwarded dstack socket but does not call the live provider matrix.

```sh
DSTACK_SOCK=/tmp/aci-dstack-sock-dev.dstack.sock \
  scripts/local_multi_upstream_smoke.sh
```

The script checks direct model routing, TLS-bound ACI-service verification, chat and embeddings receipts, attested sessions, runtime config replacement, and metrics. It tears down the Compose stack on exit unless `KEEP_STACK=1` is set.

Use this suite after changes to the ACI-service adapter, routing, receipts, sessions, config replacement, or the deployment-facing HTTP surface.

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

The provider-verifier bridge defaults `DSTACK_VERIFIER_URL` to `http://localhost:18080` when it is not already set. NEAR AI entries declare this variable as a prerequisite.

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

Strict references are allowlists, not recorded golden responses. The current strict check validates that the selected model is accepted and that the verifier emitted the expected binding type. It does not pin every claim, measurement, or evidence byte.

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
| `structured_output_max_tokens` | Token limit for the structured-output case; default `512`. |
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
| `router_refresh_smoke.py` | Middleware route refresh behavior. |
| `router_session_smoke.py` | Middleware session behavior. |
| `bfcl_v4.py` | BFCL-derived tool-calling compatibility cases. |
| `user_verify.py` | User-facing artifact verification helper. |

Read each script's `--help` output before use. These tools call live systems and can consume provider quota.

## Failure diagnosis

Start with `summary.json`, then inspect `aggregator.log` and the failing provider directory.

| Failure | First checks |
| --- | --- |
| Missing environment variable | Confirm `--env-file`, the selected matrix entry, and `api_key_env` or `requires`. |
| Missing dstack socket | Start the simulator or pass the correct `--dstack-endpoint`. |
| Provider verification failure | Inspect `provider-verifier-output.json`, then compare it with the provider policy in `docs/providers/`. |
| Binding-type mismatch | Check the matrix `binding` and the verifier's `channel_bindings`. Do not weaken the expectation without reviewing the provider protocol. |
| Gateway never becomes ready | Inspect `aggregator.log`, generated model aliases, and port ownership. |
| Receipt or session assertion failure | Compare the raw receipt, session artifact, and current canonical API schema before changing the test. |
| Strict-reference failure | Confirm the model and policy really changed before updating `provider_refs/`. |

For credential-free validation, use the project checks in [Contributing](../CONTRIBUTING.md) instead of the live suite.
