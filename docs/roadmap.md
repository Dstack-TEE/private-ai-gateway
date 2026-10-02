# Project Status and Roadmap

This page records implementation status and the next engineering priorities. It is non-normative and carries no release-date commitment.

Last reviewed: 2026-09-28.

## Implemented

- OpenAI chat, completions, embeddings, and Responses endpoints, and Anthropic Messages.
- Direct-upstream routing, and an in-process middleware path driven by an HTTP control plane, with SSE keepalives and failure accounting.
- Runtime upstream replacement: validated, redacted, atomically persisted, prewarmed, and refreshed in the background.
- Provider adapters for OpenAI-compatible, Anthropic, ACI service, Chutes, Tinfoil, NEAR AI, SecretAI, and Phala direct, with enforced TLS SPKI and Chutes E2EE key bindings.
- Fail-closed request constraints and session allowlists.
- Signed receipts, attestation reports, and immutable attested sessions.
- Multi-domain downstream TLS identities and TEE-only hostnames.
- E2EE v2 for chat, completions, and embeddings.
- Private AI Proxy: the desktop app, per-user backend, and `pap` CLI (`verify`, `audit`, `sessions`, `curl`, `send`, `serve`), with coding-agent setup.
- TypeScript packages: `@phala/aci-verifier` (browser verification, and pinned Node and Bun transports that audit receipts and sessions), `@phala/aci-provider`, and Pi and OpenCode adapters for generic ACI, RedPill, and Phala Cloud. The Pi and OpenCode adapters verify each receipt and its cited session before a response completes.
- Unit and integration tests, ACI test vectors, and a live provider suite.

Providers prove different claims; see [Provider verification](providers/README.md).

## Known limitations

These are gaps in the implementation. What ACI does not cover by design is in
[Non-goals and remaining exposure](attested-confidential-inference.md#non-goals-and-remaining-exposure).

- No public record of deployments. A client sees the release it verifies at request time, but nothing records which releases a deployment ran or when it changed.
- Neither `pap` nor the TypeScript client checks E2EE or TLS key custody, rebuilds dstack boot measurements, or accepts an exact source build. The TypeScript client also skips receipt-key custody. See [Build a verifier policy](attested-confidential-inference.md#build-a-verifier-policy).
- Receipts live in memory for one hour and are lost on restart. They do not name the downstream domain or session a request used.
- Sessions are stored as content-addressed JSONL, not in a witnessed log.
- Router providers (NEAR AI, Tinfoil, SecretAI) are trusted for per-model TEE coverage, and the instance that served a request is not identified.
- Clients check that a session's evidence matches its digest, but not what the evidence proves. See item 17 of the [conformance gap review](reviews/aci-spec-conformance-gaps.md).
- A non-`UpToDate` TCB is recorded as a refuted claim, not a failure. Relying parties enforce their own TCB policy.
- E2EE covers selected fields, not metadata. It is not available on `/v1/responses` or as an Anthropic Messages profile.
- The Private AI Proxy Local API and `pap serve` are plain local HTTP endpoints, not TEE-backed services.
- The example control plane covers only the request-decision contract. The deployment example leaves authentication, rate limiting, observability, secret delivery, and availability to the operator.

## Priorities

### Transparency

- Publish every gateway deployment and upgrade (compose hash, gateway commit, time) to an append-only transparency log, so users can audit afterward which releases served them and spot unannounced changes.
- Hash-chain or externally witness attested sessions.

### Verification

- Publish relying-party policies for TCB states, measurements, provenance, and provider roots.
- Add E2EE and TLS key custody, dstack boot measurements, and exact source builds to client verification, and receipt-key custody to the TypeScript client.
- Record the downstream domain and session in receipts, and the serving instance once a router can attest it.
- Add negative live tests for rotation, stale evidence, key expiry, and session-allowlist rejection.

### Provider pins

- NEAR AI: pin the reviewed gateway source, image, compose, and runtime policy.
- Tinfoil: pin the reviewed router release, or document why its published measurements are the complete release root.
- Chutes: pin `chute_id` per model in production, and test nonce throughput over long windows.
- Extend strict references beyond model and binding type where reviewed pins exist, and require providers to publish release material and measurements before rollout ([audit criterion 7](providers/audit-criteria.md#7-release-and-measurement-updates)).

### Operations

- A durable receipt store that survives restarts and serves multiple replicas.
- Production guidance for state backup, recovery, permissions, growth, replica ownership, health checks, and alerting.
- Metrics for verification cache health, refresh results, binding mismatches, and the Chutes nonce pool.
- Per-upstream refresh scheduling, a real nonce count on the Chutes `refreshed_via_verifier` path, and a low-watermark nonce refill.
- A pinned runner image or reviewed prebuilt binary instead of the runtime apt and rustup bootstrap.
- Multi-region identity and state: KMS application identity, receipt locality, failover, and session availability.

### API, clients, and control plane

- Native structured decisions through `POST /v1/systemone`; see the [System One design proposal](systemone-design.md). The endpoint is not implemented.
- Optional receipt and session-policy checks in `pap curl`.
- Define or reject an Anthropic Messages E2EE profile.
- Responses API conformance tests, and live coverage for streaming, tools, structured output, multimodal input, context limits, and caching.
- A production-grade reference control plane, contract tests for catalog routing, TEE-only filtering, candidate order, and failover, and documented control-plane authentication across trust boundaries.

An item moves to Implemented when code, tests, and the living docs agree. [Contributing](../CONTRIBUTING.md#documentation) lists the pages each change must update.
