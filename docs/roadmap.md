# Project Status and Roadmap

This page records implementation status and the next engineering priorities. It is non-normative and carries no release-date commitment.

Last reviewed: 2026-09-26.

## Implemented

The gateway includes:

- OpenAI-compatible chat, text completion, embeddings, and Responses endpoints;
- Anthropic Messages compatibility;
- direct-upstream routing and an in-process middleware path backed by an HTTP control plane;
- pre-first-byte SSE keepalives for unconstrained middleware streams, with
  explicit cancellation, timeout, and post-commit failure accounting;
- runtime upstream replacement with validation, redaction, atomic persistence, prewarm, and background refresh;
- provider adapters for OpenAI-compatible, Anthropic, ACI service, Chutes, Tinfoil, NEAR AI, SecretAI, and Phala direct deployments;
- request-level fail-closed verification constraints and current-session allowlists;
- enforced TLS SPKI and Chutes E2EE public-key channel bindings;
- signed ACI receipts, canonical attestation reports, and immutable attested sessions;
- multi-domain downstream TLS identities and TEE-only middleware domains;
- the E2EE v2 compatibility extension for chat completions, text completions,
  and embeddings;
- the unified Private AI Proxy CLI (`pap`, also available as `aci`) for live
  verification, offline audit, session inspection, single-request curl over an
  attested SPKI-pinned channel, one verified chat request with receipt checks,
  and a local verifying proxy;
- the Private AI Proxy desktop app, per-user backend, managed profiles, local
  API, and coding-agent integration;
- unit and integration tests, ACI test vectors, provider-verifier tooling, and
  a live provider suite;
- the public `@phala/aci-verifier` ESM package, with browser verification and
  instance-scoped Node and Bun transports that verify identity, pin TLS,
  enforce serving policy, capture wire digests, and audit receipts and cited
  sessions;
- the public `@phala/aci-provider` shared provider and native Pi and OpenCode
  adapters for generic ACI, RedPill, and Phala Cloud, for eight published npm
  packages in total;
- host-native authentication, credential and model persistence, and provider
  lifecycle in Pi and OpenCode, with gateway-authoritative model capabilities;
- automatic signed-receipt and cited-session verification before a Pi or
  OpenCode response stream completes, including consumer cancellation.

Implementation does not imply that every provider proves the same claims. Review [Provider verification](providers/README.md) and treat unproven claims as unknown.

## Known limitations

These boundaries should inform deployment decisions:

- Upstream verification is opt-in per request or route. An unconstrained request is not guaranteed to use a verified upstream.
- The `pap` CLI and the TypeScript client verify TDX quotes and the ACI
  binding chain. `pap` checks receipt-key custody when given
  `--accept-subject app-id:0x<hex>` and `--accept-dstack-kms-root-public-key`;
  the TypeScript client does not check custody. Neither client checks E2EE or
  TLS key custody, reconstructs dstack boot measurements, or accepts an exact
  source build.
- Receipt storage is in memory and expires after one hour. A process restart removes prior receipts.
- Session storage is content-addressed JSONL, not a hash-chained or externally witnessed transparency log.
- The gateway does not prove model-weight provenance unless a provider verifier supplies evidence that supports that claim. Provider mappings normally leave it unknown.
- For router providers (NEAR AI and Tinfoil), per-model TEE coverage is delegated to the verified router. Nothing establishes which backend instance served a given request.
- Receipts do not name the downstream domain or downstream session a request used.
- A non-`UpToDate` TCB value can be recorded as a refuted session claim without making every provider verifier fail. Relying parties must enforce their TCB policy.
- E2EE covers selected request and response fields, not all metadata. It is not supported on `/v1/responses`, and no native Anthropic Messages field profile is documented.
- The example control plane implements only the minimal request-decision contract. It is not a production router or a complete catalog service.
- The deployment example exposes the gateway directly and assumes the operator supplies authentication, rate limiting, observability, secret delivery, and availability controls appropriate to the environment.
- The full live matrix fails in the lifecycle and embeddings cases (`scripts/live_e2e/cases/`). They call a removed `aci` binary and assert the removed `transparency.request_modified` receipt event; see [Live end-to-end suite](live-e2e-test-suite.md). The lifecycle case also fetches the legacy `/v1/signature` route.

## Priorities

### Verification completeness

- Publish explicit relying-party policies for accepted TCB states, measurements, software provenance, and provider-specific roots.
- Extend client verifier policy to E2EE and TLS key custody, dstack boot
  measurements, and exact source-build provenance, and add receipt-key custody
  to the TypeScript client.
- Expand strict provider references beyond model and binding type where stable, independently reviewed pins are available.
- Surface request-bound, per-instance model attestation on the receipt once a router upstream can attest the instance that served a request.
- Record the downstream domain and session in receipts.
- Add negative live tests for rotation, stale evidence, key expiry, and session-allowlist rejection.

### Provider strict pins

- NEAR AI: pin the reviewed gateway source, image, compose, and runtime policy in the adapter.
- Tinfoil: pin the reviewed router release, or document why Tinfoil's published measurements are the complete release root.
- Chutes: use explicit per-model `chute_id` pins in production configurations and complete long-window nonce-throughput testing.
- Require providers to publish candidate release material and expected measurements before production rollout, as [audit criterion 7](providers/audit-criteria.md#7-release-and-measurement-updates) describes.

### Durable audit and operations

- Define a durable receipt-store interface and retention policy suitable for process restarts and multiple replicas.
- Evaluate hash chaining or external witnessing for session transparency.
- Document backup, recovery, file permissions, storage growth, and multi-replica ownership for production state.
- Add provider-verification and Chutes nonce-pool metrics for cache health,
  refresh results, binding mismatches, and pool depletion.
- Replace the runtime apt and rustup bootstrap with a pinned, gateway-owned
  runner image or reviewed prebuilt binary.
- Define multi-region identity and state behavior, including KMS application
  identity, receipt locality, failover, and session availability.
- Add deployment health, readiness, and alerting guidance based on concrete service-level objectives.

### API and client coverage

- Update `cases/lifecycle.py` and `cases/embeddings.py` to run `pap audit`, to the receipt events the gateway emits, and to canonical `/v1/aci/*` routes, keeping explicit legacy compatibility tests.
- Extend `pap curl` beyond preflight verification and SPKI pinning so supported
  API requests can also opt into receipt and session-policy checks.
- Define or reject a native Anthropic Messages E2EE profile at the API boundary.
- Expand Responses API conformance and streaming interoperability tests.
- Expand live provider coverage for streaming, tools, structured output,
  multimodal input, context limits, cache behavior, and strict release
  provenance without treating unsupported capabilities as passes.
- Keep the managed Private AI Proxy local API and standalone `pap serve`
  boundaries explicit: the local client is not itself a TEE-backed ACI service.

### Middleware and control plane

- Derive the middleware prefix-hash HMAC key from dstack KMS inside the gateway, and remove `prefix_hash_secret` and the unkeyed hash ([#315](https://github.com/Dstack-TEE/private-ai-gateway/issues/315)). An unkeyed or operator-supplied key lets an operator who also runs the control plane confirm a guessed prompt prefix.
- Provide a production-grade reference control plane or narrow the example contract further so its intended scope is unmistakable.
- Add contract tests for catalog subpaths, query preservation, TEE-only domain filtering, candidate ordering, and failover decisions.
- Document control-plane authentication and transport requirements for deployments that cross a trust boundary.

## Change discipline

A roadmap item moves to “implemented” only when code, tests, and the relevant living documentation agree. Provider observations and dated audits remain evidence records; they do not silently become protocol guarantees.

When behavior changes, update at least:

- [API reference](api-reference.md) for routes or wire behavior;
- [Configuration reference](configuration-reference.md) for fields and defaults;
- [Control-plane contract](control-plane-contract.md) for middleware decisions;
- [Provider verification](providers/README.md) for verifier claims and limitations;
- [Live end-to-end suite](live-e2e-test-suite.md) for runnable validation.
