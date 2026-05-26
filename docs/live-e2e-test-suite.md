# Live E2E Test Suite Design

This document defines the live E2E scripts for Private AI Gateway. The suite
sends real traffic to supported upstream providers,
verifies upstream attestation and channel binding, checks API fidelity, and
proves the output back to a relying party through ACI reports and receipts.
See [upstream-verification-lifecycle.md](upstream-verification-lifecycle.md)
for the current verification/session lease design and the latest Chutes
throughput findings.

## Goals

- Verify each supported provider before sending sensitive traffic.
- Verify that the gateway enforces the provider binding it accepted.
- Exercise real OpenAI-compatible traffic through the gateway, not only
  provider fixture tests.
- Prove the no-middleware path remains behavior-compatible with the current
  gateway.
- Prove the middleware path can rewrite requests and select a target route
  while backend-owned provider verification facts remain unforgeable.
- Check API fidelity for the surfaces users rely on: streaming, tool calls,
  structured outputs, multimodal inputs, context limits, and cache metadata.
- Give users a concrete verification story for "I received this API response;
  how do I know it came from the verified gateway and a verified upstream?"

## Non-Goals

- Do not make every provider pass every feature. Providers differ. The suite
  must be capability-aware.
- Do not treat a live provider metadata endpoint as automatically trusted. If
  it is not signed by the provider, a strict run uses reviewed vendored
  reference values.
- Do not hide provider bugs with response post-processing. The fidelity tests
  should show the real behavior.

## Target Script Layout

Directory:

```text
scripts/live_e2e/
  run.py
  bfcl_v4.py
  preflight.py
  provider_verify.py
  launch_gateway.py
  cases/
    lifecycle.py
    embeddings.py
    framework_no_middleware.py
    framework_middleware.py
    fidelity_text.py
    fidelity_streaming.py
    fidelity_tools.py
    fidelity_structured_outputs.py
    fidelity_multimodal.py
    fidelity_context.py
    fidelity_cache.py
  user_verify.py
  providers.json
  provider_refs/
    tinfoil.json
    near-ai.json
    chutes.json
    aci-dcap.json
examples/
  verify_aci_artifacts.rs
```

The current tree still has some older helper names, such as
`launch_aggregator.py`. Rename those as part of the framework test work rather
than treating the old names as product concepts.

`run.py` is the orchestrator. It accepts:

```bash
uv run python scripts/live_e2e/run.py --profile quick
uv run python scripts/live_e2e/run.py --profile full
uv run python scripts/live_e2e/run.py --profile strict-release
uv run python scripts/live_e2e/bfcl_v4.py --provider tinfoil
uv run python scripts/live_e2e/bfcl_v4.py \
  --provider tinfoil \
  --test-category simple_python \
  --max-cases 2
uv run python scripts/live_e2e/user_verify.py \
  --base-url https://gateway.example \
  --chat-id chatcmpl-... \
  --request-body request.json \
  --response-body response.json
cargo run --example verify_aci_artifacts -- \
  --report report.json \
  --receipt receipt.json \
  --nonce nonce-used-for-report \
  --request-body request.json \
  --response-body response.json
```

Profiles:

- `quick`: one non-streaming request per provider plus receipt verification.
- `full`: all capability-enabled fidelity cases.
- `strict-release`: full profile plus vendored reference pins, gateway code
  provenance, launcher/image provenance, and fail-closed negative checks.
- `user-verify`: no provider secrets. Verifies an already received response.

## Provider Matrix

`providers.json` is the mutable test matrix. It contains public aliases, real
upstream model ids, provider type, base URL, required env vars, and supported
fidelity cases.

Initial entries:

```json
[
  {
    "name": "tinfoil-live",
    "provider": "tinfoil",
    "base_url": "https://inference.tinfoil.sh",
    "public_model": "live-tinfoil",
    "upstream_model": "kimi-k2-6",
    "api_key_env": "TINFOIL_API_KEY",
    "binding": "tls_spki_sha256",
    "capabilities": ["chat", "streaming", "tools", "structured_outputs", "context"],
    "structured_output_max_tokens": 2048
  },
  {
    "name": "near-ai-live",
    "provider": "near-ai",
    "base_url": "https://cloud-api.near.ai",
    "public_model": "live-near",
    "upstream_model": "google/gemma-4-31B-it",
    "api_key_env": "NEARAI_API_KEY",
    "binding": "tls_spki_sha256",
    "requires": ["DSTACK_VERIFIER_URL"],
    "capabilities": ["chat", "streaming", "structured_outputs", "context"]
  },
  {
    "name": "chutes-live",
    "provider": "chutes",
    "base_url": "https://api.chutes.ai",
    "public_model": "live-chutes",
    "upstream_model": "moonshotai/Kimi-K2.5-TEE",
    "api_key_env": "CHUTES_API_KEY",
    "binding": "e2ee_public_key_sha256",
    "chutes_chute_ids": {
      "moonshotai/Kimi-K2.5-TEE": "..."
    },
    "capabilities": ["chat", "streaming", "context"]
  }
]
```

The matrix is explicit. If a provider/model does not support images, strict
JSON schema, or tools, that case is skipped for that entry instead of being
marked as passed.

## Provider Reference Policy

Each provider gets a `provider_refs/<provider>.json` reviewed reference file.
The verifier compares live evidence to this file in `strict-release` mode.
In `quick` and `full`, the verifier may accept provider-current evidence, but
it still records the evidence digest and binding in the receipt.

Reference files should contain:

```json
{
  "provider": "chutes",
  "reviewed_at": "2026-05-15",
  "expires_at": "2026-06-15",
  "source_refs": [
    "https://api.chutes.ai/servers/tee/measurements"
  ],
  "accepted_models": {
    "moonshotai/Kimi-K2.5-TEE": {
      "expected_binding": "e2ee_public_key_sha256",
      "accepted_measurement_profiles": ["..."],
      "requires_gpu_attestation": true
    }
  }
}
```

Provider-specific rules:

- Tinfoil: verify the Tinfoil attestation using the provider-owned verifier
  and vendored Tinfoil model/router metadata. The accepted binding is the TLS
  SPKI digest committed in the attestation report data.
- NEAR AI: request attestation with TLS fingerprint binding, verify the
  gateway workload through `DSTACK_VERIFIER_URL`, enforce the gateway TLS SPKI,
  then fetch a model-scoped attestation report over that verified channel and
  require non-empty `model_attestations[]`. The nested model attestations are
  hashed and recorded as gateway-owned model evidence; Private AI Gateway does not need to
  re-verify them in the normal lease path. External-provider models that cannot
  produce model evidence are skipped unless a test explicitly expects
  rejection.
- Chutes: verify the TDX report data binds `nonce || e2e_pubkey`, verify DCAP,
  verify the public measurement profile against a reviewed reference, verify
  NVIDIA evidence when present, and hash the decoded ML-KEM public-key bytes
  for the binding enforced by the transport. Strict production entries should
  pin upstream model ids to concrete `chute_id` UUIDs with `chutes_chute_ids`;
  the verifier and gateway config use the same pins.
- ACI/DCAP: verify `/v1/attestation/report?nonce=...`, quote report data,
  dstack KMS identity custody, accepted workload id or image digest, accepted
  KMS root, and attested TLS SPKI.

Reference updates must be reviewed. The script may print a proposed diff with
`--update-refs-dry-run`, but it should not silently rewrite trust pins.

## Test Phases

### 00 Preflight

Checks:

- Required API keys are present but never printed.
- `PRIVATE_AI_VERIFIER_DIR` exists or the sibling verifier checkout exists.
- `DSTACK_VERIFIER_URL` responds for NEAR AI and ACI/DCAP tests.
- `PRIVATE_AI_GATEWAY_DSTACK_ENDPOINT` or a local dstack socket exists for
  gateway attestation tests.
- The gateway binary builds.
- No live server is already bound to the selected local port.

### 10 Provider Attestation

For each provider:

- Run the provider verifier directly.
- Assert `result == verified`.
- Assert `evidence_digest`, `evidence_ref`, and at least one channel binding.
- Assert binding type matches the provider transport.
- In strict mode, compare evidence claims to `provider_refs/<provider>.json`.

Negative checks:

- Mutate the TLS SPKI or E2EE public key binding and assert forwarding fails.
- For Chutes, mutate `key_id` or public-key digest and assert `/e2e/invoke`
  is never called.

### 20 Gateway Launch

The script writes a temporary upstream config from `providers.json`, starts a
local gateway against the real dstack socket, and deletes the config on
exit because it contains live bearer tokens.

Checks:

- `GET /v1/models` returns only public aliases.
- `GET /v1/attestation/report?nonce=<random>` verifies:
  - workload id equals hash of the attested identity public key,
  - keyset endorsement verifies,
  - quote report data binds the ACI attestation statement,
  - dstack KMS custody verifies when using dstack,
  - source provenance fields match the expected repo/commit or image digest,
  - attested TLS SPKI is present when configured.

### 30 Lifecycle

For each provider and enabled request mode:

- Send request through the gateway.
- Assert response status and OpenAI-compatible shape.
- Fetch `/v1/receipt/{chat_id}` with the original bearer token.
- Verify receipt signature using the receipt key from the attested keyset.
- Verify receipt workload id and keyset digest match the attestation report.
- Verify `request.received` hash equals the exact client body.
- Verify `request.forwarded` hash equals the model-rewritten upstream body.
- Verify `transparency.request_modified` exists when model alias rewriting
  happened.
- Verify `upstream.verified` is `verified`, `required == true`, has evidence
  digest/ref, and has enforceable binding material.
- Verify `response.returned.cleartext_hash` equals the response body for
  non-streaming requests or the reassembled ordered SSE bytes for streaming.

The first runnable slice covers the non-streaming lifecycle and relying-party
verification path. The verifier intentionally uses the Rust protocol code for
ACI canonicalization, keyset binding, and receipt signature checks instead of
reimplementing those rules in Python.

### 32 Embeddings

Capability-gated on `embeddings`. For each provider that lists it, the runner:

- Sends `POST /v1/embeddings` through the gateway with a fixed `input` string.
- Asserts the OpenAI-compatible response shape (`object: "list"`, non-empty
  `data[]` with a numeric `embedding[]` whose components are not all zero).
- Fetches `/v1/receipt/{receipt_id}` using the `x-receipt-id` header value as
  the lookup id, since OpenAI embeddings responses carry no `id` field. The
  gateway's receipt endpoint accepts either `chat_id` or `receipt_id` as the
  path parameter.
- Runs the same `verify_aci_artifacts` example against the receipt + request +
  response bodies to confirm canonical request/forwarded/response hashes,
  receipt signature, and channel binding.
- Asserts `receipt.endpoint == "/v1/embeddings"` and that `upstream.verified`
  carries the provider's declared binding (e.g. `e2ee_public_key_sha256` for
  Chutes embeddings).

The first wired model is `Qwen/Qwen3-Embedding-8B-TEE` on Chutes (chute_id
`21822836-bfa6-5426-b27e-dd5fdda1249e`), routed via the same
`ChutesProviderBackend` E2EE path as chat. There is currently no Phala-deployed
TEE embedding model in `Dstack-TEE/vllm-proxy` (the proxy does not register a
`/v1/embeddings` route yet), and no Tinfoil/NEAR embedding entry — the matrix
will expand once those land.

### 35 Framework Compatibility

Run the same lifecycle cases in two gateway modes:

- **No middleware:** frontend calls backend directly. Assert behavior matches
  the current request path: `body.model` is the target route id, backend rewrites
  to the upstream model, and receipts contain the same verification and hash
  facts as today.
- **Fixture middleware:** frontend forwards plaintext to a local middleware
  fixture. The fixture rewrites the request and selects a different configured
  target route id. Backend must validate that route, verify the provider, and
  record backend-authored route/provider facts that the middleware cannot forge.

Middleware fixture checks:

- Public requests with forged `X-Private-AI-Gateway-*` headers are sanitized by
  the frontend.
- Middleware cannot claim `upstream.verified`; backend must author that event.
- E2EE AAD uses the original user model even when middleware selects a
  provider-qualified target route.
- The final receipt distinguishes `request.received`, `middleware.forwarded`,
  `route.selected`, `upstream.forwarded`, `upstream.verified`, and
  `response.returned`.

The `bfcl_v4.py` wrapper runs the Berkeley Function Calling Leaderboard v4
through the local gateway over OpenAI-compatible Chat Completions. It is
intentionally separate from `run.py`: BFCL is useful for native tool-call
fidelity and multi-turn agentic behavior, while the ACI runner remains the
source of truth for report, receipt, and upstream binding verification.

Some models need provider-specific output budgets even when they support the
same OpenAI-compatible feature. Tinfoil `kimi-k2-6` emits a large
`message.reasoning` field before final `message.content`; live tests showed
that 512 completion tokens can stop in reasoning-only output, while the same
schema succeeds with a larger budget. The provider matrix keeps this as an
explicit per-provider test parameter instead of rewriting provider responses.

By default the BFCL wrapper expects a sibling checkout at
`../gorilla-bfcl/berkeley-function-call-leaderboard`, or an explicit
`--bfcl-dir`. It deterministically samples a spread of 1% of BFCL v4
`single_turn` and `multi_turn` cases with a two-case-per-category floor because
BFCL's leaderboard CSV step computes latency standard deviation. Random
sampling is available only with `--sample-mode random`. The wrapper exposes
each tested provider under a BFCL-supported OpenAI Chat Completions model key,
then runs BFCL's own `generate` and `evaluate` commands with `--run-ids` and
`--partial-eval`. When `--max-cases` caps a broad category group, the cap keeps
a deterministic spread of categories instead of taking the first categories
greedily.

`scripts/live_e2e/providers.glm51.json` maps the currently shared GLM 5.1
family across Tinfoil, NEAR AI, and Chutes. It is the first cross-provider BFCL
matrix because it exercises the same broad model family while still preserving
each provider's native attestation and transport path.

Useful tool-call fidelity slice:

```bash
python3 scripts/live_e2e/bfcl_v4.py \
  --providers-file scripts/live_e2e/providers.glm51.json \
  --test-category single_turn,multi_turn \
  --max-cases 20 \
  --no-build
```

### 40 Fidelity Cases

These cases are capability-gated per provider/model.

Text baseline:

- Deterministic short instruction with `temperature: 0`.
- Assert valid OpenAI response shape, stable `id`, `object`, `model`, `choices`,
  `finish_reason`, and `usage` when provided.

Streaming:

- Same request with `stream: true`.
- Parse every SSE frame.
- Assert chunk ids are consistent, `[DONE]` arrives, final receipt exists, and
  receipt response hash covers the exact ordered stream bytes.

Tool calls:

- Use OpenAI `tools` shape and force a specific tool with `tool_choice`.
- Assert `finish_reason == tool_calls`.
- Assert tool name and arguments parse as JSON and match the requested schema.
- For streaming tools, assert incremental chunks reconstruct the same call.

Structured outputs:

- Use `response_format: { "type": "json_schema", ... }` where supported.
- Assert the returned content parses and validates against the schema.
- Also run `response_format: { "type": "json_object" }` for models that only
  support JSON mode.

Multimodal:

- Use a tiny deterministic base64 PNG with embedded text or simple colored
  geometry.
- Assert the answer identifies the expected text/color/count.
- If a model does not support image input, skip rather than fail.
- PDF/audio/video should be separate optional cases only after the provider
  matrix has models that explicitly support those inputs.

Context:

- Send a large deterministic prefix with sentinels at the beginning, middle,
  and end.
- Ask the model to return the sentinels only.
- The test size is provider-specific and starts below the advertised context
  limit. It should not infer a provider's true maximum from one failure.

Cache:

- For provider prompt cache, keep the reusable prefix deterministic and put
  the varying question at the end.
- Use provider-supported cache controls only for models that advertise support.
- Assert cache metadata such as cached token counts or provider cache headers
  when exposed. If no metadata is exposed, report "unobservable" instead of
  passing.
- Response-cache semantics, if we add them later, are tested separately from
  provider prompt caching because they operate at a different layer.

### 50 Direct Provider Comparison

For each capability case, optionally send the same request directly to the
provider using the upstream model id and provider transport. Compare normalized
invariants:

- Response shape and required fields.
- Tool call name and JSON arguments.
- Structured-output schema validity.
- SSE parseability and completion.
- Usage fields and cache fields if the provider exposes them.

Do not require exact natural-language text equality. Use exact equality only
for structured values the prompt constrains.

## User Verification Story

`user_verify.py` is the script we should document for users.

Inputs:

- Gateway base URL.
- Chat id or receipt id.
- Original request body, optional.
- Response body or captured stream bytes, optional.
- Optional expected repo commit, image digest, TLS SPKI, or workload id.

Procedure:

1. Fetch `GET /v1/attestation/report?nonce=<random>`.
2. Verify the report binding, quote, keyset endorsement, source provenance,
   and optional TLS SPKI.
3. Fetch `GET /v1/receipt/{chat_id}`.
4. Verify receipt signature under the attested receipt key.
5. Verify receipt workload id and keyset digest match the verified report.
6. Verify request/response hashes when bodies are supplied.
7. Inspect `upstream.verified` and show provider, model id, verifier id,
   evidence digest/ref, result, and binding type.

The final output should be a human-readable summary plus a machine-readable
JSON result:

```json
{
  "verified": true,
  "workload_id": "sha256:...",
  "source_provenance": {"repo_url": "...", "repo_commit": "..."},
  "receipt": {"chat_id": "...", "signature_valid": true},
  "upstream": {
    "vendor": "chutes-live",
    "model_id": "moonshotai/Kimi-K2.5-TEE",
    "verifier_id": "private-ai-verifier/chutes/v1",
    "binding": "e2ee_public_key_sha256"
  }
}
```

## OpenRouter-Derived Fidelity Checklist

OpenRouter's compatibility surface is useful because it has to normalize many
providers. Our suite should explicitly cover the same classes of behavior:

- Standard chat parameters: `max_tokens`, `temperature`, `stop`, `seed`, and
  penalty fields where providers accept them.
- Tool calling and `tool_choice`.
- Parallel tool calls when supported.
- `response_format` JSON mode and strict JSON schema.
- Streaming and non-streaming parity.
- Multimodal message content: at minimum `image_url`, later `file`,
  `input_audio`, and `video_url` when supported providers are available.
- Context length behavior under large prompts.
- Prompt-cache metadata when a provider exposes it.

## Minimum Implementation Order

1. `user_verify.py` for already captured responses.
2. `provider_verify.py` plus `provider_refs`.
3. `run.py --profile quick` for Tinfoil, NEAR AI, Chutes.
4. Framework no-middleware compatibility case.
5. Framework fixture-middleware case with route selection and rewrite receipts.
6. Streaming and receipt hash verification.
7. Tool and structured-output cases.
8. Multimodal and context cases.
9. Cache observability.
10. Strict-release source provenance and launcher/image provenance checks.
