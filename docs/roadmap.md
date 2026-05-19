# Private AI Gateway Roadmap

Date: 2026-05-19 UTC.
Current phase: hardening a feature-complete prototype into a strict review
candidate.

This document is the aggregator-local progress tracker. The ACI spec defines
the protocol. This repo proves an adoptable implementation: OpenAI-compatible
surface, ACI receipts, dstack identity, upstream verification, and provider
adapters that fail closed when binding material cannot be enforced.

## Status Table

| Area | Status | Notes |
| --- | --- | --- |
| OpenAI-compatible chat/completions surface | Done | `/v1/chat/completions`, `/v1/completions`, streaming, E2EE addon, legacy aliases, and vLLM-compatible error behavior are covered by tests. |
| Model routing and runtime config | Done | One upstream config file, admin `GET`/`PUT`, model alias rewrite before verification/forwarding/receipt hashing. |
| ACI identity and self-attestation | In progress | dstack KMS-backed identity, keyset endorsement, TLS SPKI publication, and local dstack simulator support are implemented. Launcher provenance is tracked separately but still part of the release story. |
| Receipts and transparency events | In progress | Request/response/body hashes, streaming hashing, upstream verification events, rewrite events, and legacy `/v1/signature` alias are implemented. Persistent storage decision is still open. |
| Upstream verification lifecycle | In progress | Startup prewarm, background verification refresh, and Chutes session refresh exist. Provider soundness review is still strict-release work. |
| Provider adapters | In progress | Tinfoil, NEAR AI, and Chutes have concrete adapters. OpenAI-compatible and ACI/DCAP paths remain useful for deployment bring-up and internal dstack upstreams. |
| Live E2E fidelity suite | In progress | BFCL/OpenAI-compatible harness exists. Strict profiles and broader fidelity coverage remain P0 before external review. |
| Production operations | Next | Durable stores, deployment docs, metrics review, multi-region behavior, and rate-limit/load tests follow the strict-release pass. |

## P0 Queue

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
- Live E2E: split quick/full/strict profiles and make the strict profile cover
  tool calls, structured output, media input, context size, cache-affinity
  behavior where observable, streaming, and receipts.
- Receipts: decide the persistent receipt/body store boundary. The current
  in-memory store is acceptable only for prototype and short-lived tests.
- Launcher: finish the end-to-end provenance/attestation guide and link it from
  the aggregator deployment path.

## Provider Soundness

Supported providers must pass the criteria in
[reviews/providers/audit-criteria.md](reviews/providers/audit-criteria.md).
The current provider reports are:

- [reviews/providers/tinfoil-router-mode.md](reviews/providers/tinfoil-router-mode.md)
- [reviews/providers/near-ai-router-mode.md](reviews/providers/near-ai-router-mode.md)
- [reviews/providers/chutes-e2ee.md](reviews/providers/chutes-e2ee.md)

The implementation should stay minimal: each provider adapter owns its
transport and verification rules. The config selects a provider and model map;
it does not expose arbitrary verifier commands or policy DSLs.

## References

- [README.md](../README.md)
- [live-e2e-test-suite.md](live-e2e-test-suite.md)
- [upstream-verification-lifecycle.md](upstream-verification-lifecycle.md)
- [router-mode-provider-review.md](router-mode-provider-review.md)
