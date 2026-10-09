# System One endpoint: design proposal

Status: implemented.

Reviewed: 2026-10-02. Gateway revision: `ef26e16d222a238e68633dd3522fde9f62ddae4f`.

This maintainer record proposes `POST /v1/systemone`. The
[HTTP API reference](api-reference.md) remains the source for implemented routes.

## Decision

Support the native structured-decision protocol already published by
[TypeSafe](https://docs.typesafe.ai/api), exposed by
[OpenRouter](https://openrouter.ai/docs/api/api-reference/systemone/submit-a-system-one-request),
and served by [Kev](https://github.com/jaredpalmer/kev).

Add an endpoint to the existing inference pipeline, not a separate proxy or
decision engine. The required changes are the route, endpoint dispatch, native
field tables, and one native-support check in the existing candidate loop.
Do not add a provider format, control-plane field, attestation-provider type,
chat bridge, business validator, or retry policy.

## Contract and boundaries

| Direction | Native fields |
| --- | --- |
| Request | `model`, `state`, `questions` |
| Response | `model`, `answers`, `usage` |

Support `choice`, `score`, and `noul` questions without converting them to chat.
Preserve structured state, question IDs, option order, instructions, criteria,
and complete answers. Provider-specific limits and question semantics belong
to the upstream; do not recompute scores, probabilities, or confidence.

Keep the existing authentication, body limit, `provider` routing extension, and
[ACI request constraints](api-reference.md#require-aci-verification).
Use Embeddings' existing `force_buffered` behavior and Responses' existing
`e2ee_unsupported_endpoint` rejection, without a new streaming rewrite.

Streaming, an E2EE profile, model evaluation, deployment, and provider admission
are out of scope. Target the core protocol, not every provider extension.
Keep `GET /v1/models` unchanged; discovery and full SDK compatibility are
separate work.

## Integration

### Routing and request shaping

Add the path, handler, and `Endpoint::SystemOne` to the existing dispatch.
Reuse the common HTTP, service, verification, metering, and receipt lifecycle.

Keep the [pre-consult contract](control-plane-contract.md#post-consultpre)
unchanged. Use `format: "openai"` with a native parameter table; do not inject
chat streaming, reasoning, or engine parameters. Do not derive chat features
or token estimates from `state` and `questions`.

A candidate's `supportedEndpoints`, when non-empty, is the complete set of
paths it serves; an omitted or empty list serves the default chat, completions,
embeddings, and messages paths. [`build_candidates`](../src/middleware/request_transform.rs)
skips a candidate that does not serve the path it would be called on, so a
System One request reaches only candidates that list `/v1/systemone`, and a
chat request never reaches a native-only System One upstream. If none qualify,
the request returns `404 model_not_found` without forwarding or falling back
to chat. Do not rely on an incompatible upstream returning a retryable error.

### Responses and shared policies

- Direct-upstream mode retains existing buffered passthrough and generic remaps,
  without adding middleware identity rewriting, canonicalization, or pricing.
- Middleware mode retains existing JSON decoding, public-model identity,
  pricing, and sanitized errors. Add a canonicalizer retaining top-level
  `model`, `answers`, `usage`, and shared sanitized in-band `error`, with
  `input_tokens`, `output_tokens`, and existing `cost` handling under usage.
  Preserve `answers` without nested allowlists.
- Keep post-consult usage and failure accounting unchanged, with raw usage
  captured before cost injection and no state, questions, or answers reported.

Reuse [shared error and capacity classification](../src/middleware/errors.rs)
and [candidate failure handling](../src/aggregator/service/middleware.rs).
Do not define endpoint-specific status mappings, buffering limits, or retries.

### ACI

Direct serving is the attested workload role, not merely the direct-upstream
HTTP mode. It satisfies `provider.aci_verified` by construction and has no
upstream verifier event or session; a pinned upstream session allowlist still
fails closed. Non-direct routes require the existing verifier result and
enforceable binding whenever request or TEE-only policy demands them.

Preserve request observations, provider-facing recording, receipt bytes,
ownership, sessions where applicable, and signed refusals. No ACI wire or
receipt-schema changes are proposed.

## Acceptance

Use credential-free fixtures to verify:

- native payload preservation and topology-specific response behavior;
- unsupported candidate skipped, later supported candidate selected, and no
  forwarding when all candidates are unsupported;
- native usage, shared errors, buffered operation, and unsupported E2EE;
- one ACI success with matching receipt hashes and one fail-closed rejection,
  reusing existing shared ACI suites;
- mixed-primitive calls through a pinned official TypeSafe Python SDK.

Existing endpoint, catalog, and receipt behavior must remain unchanged.
Keep the [HTTP API reference](api-reference.md), [control-plane contract](control-plane-contract.md),
and [roadmap](roadmap.md) in sync with code and tests.
