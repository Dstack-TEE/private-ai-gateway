# Private AI Gateway Roadmap

Date: 2026-06-03 UTC.
Current phase: refactoring the feature-complete prototype into a gateway
framework, then hardening it into a strict review candidate.

This document is the gateway-local progress tracker. The ACI spec defines
the protocol. This repo proves an adoptable implementation: OpenAI-compatible
surface, ACI receipts, dstack identity, upstream verification, and provider
adapters that fail closed when binding material cannot be enforced.

## Status Table

| Area | Status | Notes |
| --- | --- | --- |
| OpenAI-compatible chat/completions surface | Done | `/v1/chat/completions`, `/v1/completions`, streaming, E2EE addon, legacy aliases, and vLLM-compatible error behavior are covered by tests. |
| OpenAI-compatible embeddings surface | Done | `/v1/embeddings` forwards through the same receipt/attestation pipeline as chat. Buffered-only (client-sent `stream:true` is forced back to buffered). ACI v2 + dstack-vllm-proxy legacy v1/v2 E2EE encrypt the `input` request field and each `data[].embedding` response field; AAD shape mirrors completions (`field=input` / `field=input.{N}` request, `data={index}|field=embedding` response). Provider adapters in this slice: openai-compatible only — Chutes embeddings (TEI native paths, not `/v1/embeddings`) and Tinfoil/NEAR-AI embedding routes still need adapter work. |
| Model routing and runtime config | Done | One upstream config file, admin `GET`/`PUT`, model alias rewrite before verification/forwarding/receipt hashing in no-middleware mode. |
| ACI identity and self-attestation | In progress | dstack KMS-backed identity, keyset endorsement, TLS SPKI publication, and local dstack simulator support are implemented. Launcher provenance is tracked separately but still part of the release story. |
| Receipts and transparency events | In progress | Request/response/body hashes, streaming hashing, upstream verification events, middleware route events, rewrite events, and legacy `/v1/signature` alias are implemented. Persistent storage decision is still open. |
| Attested sessions | In progress | Upstream verified TLS/SPKI or provider E2EE bindings now create session ids, audit records, and receipt references. Downstream session ids are pending TLS/domain binding work. |
| Upstream verification lifecycle | In progress | Startup prewarm, background verification refresh, and Chutes session refresh exist. Provider soundness review is still strict-release work. |
| Provider adapters | In progress | Tinfoil, NEAR AI, and Chutes have concrete adapters. OpenAI-compatible and ACI/DCAP paths remain useful for deployment bring-up and internal dstack upstreams. |
| Frontend/middleware/backend framework | In progress | Internal request context with expiry, out-of-band target route selection, internal backend endpoint, runtime UDS middleware mode, middleware `/v1/models` pass-through, and stream-preserving middleware transport are implemented. Production compose is still pending. |
| Multi-domain downstream TLS binding | Planned | Support multiple downstream custom domains by mapping a requested domain to its certificate/SPKI binding and publishing the matching binding in the gateway evidence path. |
| Local backend proxy mode | Planned | Let an end user run the verified-provider backend as a laptop-local OpenAI-compatible proxy without local TEE requirements. |
| Live E2E fidelity suite | In progress | BFCL/OpenAI-compatible harness exists. Strict profiles and broader fidelity coverage remain P0 before external review. |
| Production operations | Next | Durable stores, deployment docs, metrics review, multi-region behavior, and rate-limit/load tests follow the strict-release pass. |

## Pending Tasks

### P0: Attested Sessions and Audit Log

An attested session is a connection or application-level encryption context that
has been verified against attestation evidence and enforceable binding material.
Both downstream user sessions and upstream provider sessions should use this
concept.

- Define the session record shape: session id, direction, upstream/provider,
  verification time, expiry, byte-preserving verifier evidence, verified claim
  tags, and enforceable session binding material. Implemented for upstream
  sessions.
- Treat TLS with SPKI pinning and provider/client E2EE as supported binding
  types. Implemented for upstream sessions.
- Write each successful upstream session verification to an audit log that can
  be queried by session id. Implemented at
  `GET /v1/audit/sessions/{session_id}`.
- Make receipts reference the upstream session id used for the request.
  Implemented as `upstream.verified.session_id` when a verified binding exists.
- Add downstream session ids once the gateway can select and report
  domain-specific TLS bindings.
- Keep the implementation small: reuse the existing upstream lease lifecycle
  where possible, and avoid introducing a policy DSL.

### P0: Multi-Domain Downstream TLS Binding

The gateway currently assumes one downstream TLS identity. Production deployments
may need multiple custom domains bound to the same gateway workload.

- Add runtime config for a domain-to-certificate mapping.
- Select the served certificate from the requested domain, using the TLS SNI
  value where available and the HTTP host only for evidence/report selection.
- Publish the SPKI binding for the requested downstream domain in the gateway
  attestation/evidence response.
- Ensure receipts and attested-session audit records identify the downstream
  domain/session used by the request.
- Keep certificate issuance and renewal out of scope for this repo; another
  component may mount certificates for the gateway to serve.

### P0: Frontend / Middleware / Backend Refactor

Source design: [frontend-middleware-backend.md](frontend-middleware-backend.md).

- Introduce internal request context keyed by `request_id`, with expiry for
  pending middleware requests. Implemented.
- Split the current request path into frontend preparation, backend
  verification/forwarding, and frontend response finalization. Implemented for
  the current UDS middleware path, including streaming response finalization.
- Keep middleware-disabled mode as the default and prove it preserves current
  behavior. Implemented and covered by the full test suite.
- Add a local backend endpoint or in-process backend callable guarded by
  request context lookup. Implemented as a separate internal router builder and
  runtime listener when middleware is enabled.
- Add optional UDS middleware mode with a fixture middleware for tests.
  Implemented through `PRIVATE_AI_GATEWAY_MIDDLEWARE_UDS_PATH`.
- Ensure external `X-Private-AI-Gateway-*` headers cannot steer the public
  frontend. Implemented by generating internal context server-side; covered by
  tests.
- Make backend validate target route ids and reject arbitrary upstream URLs.
  Implemented for in-process route selection.
- Record route/backend receipt facts from backend observations, not middleware
  claims. Implemented with `middleware.forwarded`, `route.selected`, and final
  `request.forwarded` events.
- Finalize middleware-mode receipts in the frontend after middleware returns,
  with backend-owned `response.received` and frontend-owned
  `response.returned`. Implemented.
- Add E2EE tests proving ACI v2 response AAD uses the frontend-observed user
  model when middleware selects a separate target route. Implemented for the
  current UDS middleware path.
- Update deploy docs after the middleware mode has a concrete production compose
  shape.
- Add production compose wiring for a middleware container.
- Keep the middleware developer contract current in
  [middleware-integration.md](middleware-integration.md). Initial guide is
  written.

### P0: Provider Soundness and Strict Pins

- NEAR AI: pin reviewed gateway source/image/compose provenance and runtime
  policy, then document the exact release accepted by the adapter.
- Tinfoil: move from "provider-current verifier result" to a strict release
  pin for the reviewed router digest/release, or document why the provider's
  published measurements are the complete release root.
- Provider release process: require supported gateway/router providers to
  publish candidate source/release material and expected measurements before
  production rollout, so strict verifiers can review and pin upgrades without
  blindly trusting new workloads.
- Chutes: use explicit per-model `chute_id` pins in production configs and
  complete long-window nonce-throughput testing.
- SecretAI: review complete (SEV-SNP + NVIDIA Hopper, single-VM trust
  boundary; see [reviews/providers/secret-ai.md](reviews/providers/secret-ai.md)).
  Adapter implementation deferred until SCRT addresses partner feedback sent
  2026-05-23 — SPKI binding, per-release build provenance, downstream image
  digest pins, journald policy, and open-sourcing `secret-vm-attest-rest-server`
  (feedback: <https://hackmd.io/@h4x3rotab/H1b2ECA1Ml>). Resume by adding
  `UpstreamProvider::SecretAi` and `SecretAiProviderVerifier` parallel to
  the existing Chutes/Tinfoil/NEAR adapters; the review's "Required Adapter
  Behavior" section captures wiring requirements.

### P0: Live E2E and User Verification

- Split quick/full/strict profiles in the live E2E suite.
- Add framework tests for no-middleware compatibility and fixture middleware
  route selection.
- Make strict profile cover tool calls, structured output, media input, context
  size, cache-affinity behavior where observable, streaming, receipts, and
  source/launcher provenance.
- Finish the user verification script for already captured responses.

### P1: Local Backend Proxy Mode

- Add a mode that runs only the verified-provider backend as a local
  OpenAI-compatible proxy for end users and agents. This mode should not require
  a local TEE, dstack KMS, or gateway self-attestation because the process runs
  on the user's own machine and is part of the user's local trust boundary.
- Reuse the same provider adapters, upstream verification lifecycle, and
  transport/session binding logic as the gateway backend. The local proxy must
  fail closed when the upstream provider cannot be verified or when the verified
  binding cannot be enforced.
- Keep the configuration minimal: local bind address, upstream config path, and
  provider credentials. Avoid adding a separate verifier DSL or local policy
  system.
- Document the trust model clearly: local proxy mode verifies upstream
  providers for the local user, but it does not claim to provide a TEE-backed
  ACI service identity to downstream clients.

### P1: Production State and Operations

- Decide the persistent receipt/body store boundary. The current in-memory
  store is acceptable only for prototype and short-lived tests.
- Add durable provider lease/session observability and Chutes nonce pool
  metrics.
- Replace runtime apt/rustup bootstrap with a gateway-owned runner image or
  prebuilt binary image.
- Define multi-region behavior: replicated KMS app id, receipt locality, and
  retained-body storage.

## Provider Soundness

Supported providers must pass the criteria in
[reviews/providers/audit-criteria.md](reviews/providers/audit-criteria.md).
The current provider reports are:

- [reviews/providers/tinfoil-router-mode.md](reviews/providers/tinfoil-router-mode.md)
- [reviews/providers/near-ai-router-mode.md](reviews/providers/near-ai-router-mode.md)
- [reviews/providers/chutes-e2ee.md](reviews/providers/chutes-e2ee.md)
- [reviews/providers/secret-ai.md](reviews/providers/secret-ai.md)

The implementation should stay minimal: each provider adapter owns its
transport and verification rules. The config selects a provider and model map;
it does not expose arbitrary verifier commands or policy DSLs.

## References

- [README.md](../README.md)
- [live-e2e-test-suite.md](live-e2e-test-suite.md)
- [frontend-middleware-backend.md](frontend-middleware-backend.md)
- [middleware-integration.md](middleware-integration.md)
- [upstream-verification-lifecycle.md](upstream-verification-lifecycle.md)
- [router-mode-provider-review.md](router-mode-provider-review.md)
