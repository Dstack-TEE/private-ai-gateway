# Project Status and Roadmap

This page records current implementation status and the next engineering priorities. It is non-normative and carries no release-date commitment.

Last reviewed: 2026-08-31.

## Implemented

The current gateway includes:

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
- the `aci` CLI for live verification, offline audit, session inspection,
  arbitrary curl requests over an attested SPKI-pinned channel, one verified
  chat request with receipt checks, and a local verifying proxy;
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

These boundaries are present in the current code and should inform deployment decisions:

- Upstream verification is opt-in per request or route. An unconstrained request is not guaranteed to use a verified upstream.
- The Rust CLI and TypeScript client verify TDX quotes and the ACI binding
  chain, but they do not reconstruct dstack boot measurements or implement a
  complete private-key-custody and exact source-build acceptance policy.
- Receipt storage is in memory and expires after one hour. A process restart removes prior receipts.
- Session storage is content-addressed JSONL, not a hash-chained or externally witnessed transparency log.
- The gateway does not prove model-weight provenance unless a provider verifier supplies evidence that supports that claim. Current provider mappings normally leave it unknown.
- A non-`UpToDate` TCB value can be recorded as a refuted session claim without making every provider verifier fail. Relying parties must enforce their TCB policy.
- E2EE covers selected request and response fields, not all metadata. It is not supported on `/v1/responses`, and no native Anthropic Messages field profile is documented.
- The example control plane implements only the minimal request-decision contract. It is not a production router or a complete catalog service.
- The deployment example exposes the gateway directly and assumes the operator supplies authentication, rate limiting, observability, secret delivery, and availability controls appropriate to the environment.
- The live end-to-end runner still uses legacy report and receipt-wrapper routes for parts of its compatibility checks.

## Priorities

### Verification completeness

- Publish explicit relying-party policies for accepted TCB states, measurements, software provenance, and provider-specific roots.
- Complete the client verifier policy for dstack boot measurements,
  private-key custody, and exact source-build provenance.
- Expand strict provider references beyond model and binding type where stable, independently reviewed pins are available.
- Add negative live tests for rotation, stale evidence, key expiry, and session-allowlist rejection.

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

- Move live verification cases to canonical `/v1/aci/*` artifacts while retaining explicit legacy compatibility tests.
- Extend `aci curl` beyond preflight verification and SPKI pinning so arbitrary
  supported API requests can also opt into receipt and session-policy checks.
- Define or reject a native Anthropic Messages E2EE profile at the API boundary.
- Expand Responses API conformance and streaming interoperability tests.
- Expand live provider coverage for streaming, tools, structured output,
  multimodal input, context limits, cache behavior, and strict release
  provenance without treating unsupported capabilities as passes.
- Decide whether to ship the planned backend-only local proxy that reuses the
  provider verifiers and transports without claiming a local TEE-backed ACI
  service identity. This is distinct from `aci serve`, which verifies and
  proxies an existing remote ACI service.

### Middleware and control plane

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
