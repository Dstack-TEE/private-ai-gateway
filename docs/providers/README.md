# Provider verification

This section documents the TEE provider adapters that can produce verified upstream sessions. It is for operators setting acceptance policy and reviewers auditing what each `verified` result actually proves.

Each provider has a living `verification.md` page for the current adapter and, once audited, a dated `review.md` admission record; see [Documentation conventions](../README.md#documentation-conventions). A verification page states the evidence endpoints and freshness mechanism, the mandatory rejection checks, the value bound into hardware evidence and how forwarding enforces it, the typed session claims and their sources, supplemental evidence, tests, and limitations. A verifier change updates its verification page in the same change, as described in [Contributing](../../CONTRIBUTING.md#provider-verification).

## Provider matrix

| Provider | Attested boundary | Enforced binding | Living reference | Dated audit decision |
| --- | --- | --- | --- | --- |
| 0G Private Computer | Per-provider 0G broker TEE (dstack), private tier only | `e2ee_public_key_sha256` | [Verification](0g-private-computer/verification.md) | [Acceptable with conditions](0g-private-computer/review.md), 2026-10-20 |
| ACI service | ACI-compatible dstack service | `tls_spki_sha256` | [Verification](aci-service/verification.md) | First-party path; no separate audit |
| ArmetAI | Per-model Intel TDX + NVIDIA CC GPU endpoint (non-dstack), measurement-pinned | `tls_spki_sha256` | [Verification](armet-ai/verification.md) | Not yet audited; [self-assessment](armet-ai/review.md), 2026-09-29 |
| Chutes | Per-instance Intel TDX workload | `e2ee_public_key_sha256` | [Configuration](chutes/configuration.md), [verification](chutes/verification.md) | [Accepted for limited traffic](chutes/review.md), 2026-05-18 |
| NEAR AI | Intel TDX router gateway | `tls_spki_sha256` | [Verification](near-ai/verification.md) | [Acceptable with conditions](near-ai/review.md), 2026-05-18 |
| Phala direct | Per-model dstack-vllm-proxy endpoint | `tls_spki_sha256` | [Verification](phala-direct/verification.md) | [Acceptable with conditions](phala-direct/review.md), 2026-06-10 |
| Privatemode | Official proxy co-deployed in the gateway's measured Compose | `proxy_image_sha256` | [Verification](privatemode/verification.md) | [Acceptable with conditions](privatemode/review.md), 2026-05-26 |
| SecretAI | SecretVM router workload | `tls_spki_sha256` | [Verification](secret-ai/verification.md) | [Acceptable with conditions](secret-ai/review.md), 2026-05-22 |
| Tinfoil | Confidential model router | `tls_spki_sha256` | [Verification](tinfoil/verification.md) | [Acceptable with conditions](tinfoil/review.md), 2026-05-18 |

`openai-compatible` and `anthropic` are supported transport adapters, but they do not create verified TEE sessions.

## Candidate admission records

These dated reviews record admission decisions for providers outside the
implemented adapter matrix above.

| Provider | Reviewed boundary | Dated audit decision |
| --- | --- | --- |
| Confidential AI | c8s front door on TDX node-as-CVMs; workers trusted transitively | [Not acceptable](confidential-ai/review.md), 2026-10-27 PDT (round 3) |

## What `verified` means

A provider verifier returns `verified` only with at least one enforceable channel binding, and the gateway forwards a prompt only over a connection that enforces that binding; [Request-time flow](../upstream-verification-lifecycle.md#request-time-flow) describes the steps. The check lives in `src/aci/verifier/`, `src/aci/upstream/`, and `src/aggregator/service/forward.rs`, and `tests/upstream_verifier.rs::service_fails_if_selected_backend_cannot_enforce_channel_binding` covers the fail-closed case.

`verified` does not assert every typed session claim. Each verification page lists the claims its adapter asserts, and [Attested sessions](../attested-session-system.md#how-claims-are-derived) explains how claims and their sources are derived.

The common audit rubric is [Provider audit criteria](audit-criteria.md). The cross-provider router reviews are [Router-mode soundness](../reviews/router-mode-soundness.md) and [Router load balancing and cache](../reviews/router-mode-load-balancing-cache.md), and [Router-mode provider review](../reviews/router-mode-provider-review.md) records how those reviews were run.

## Audit a request

Check the receipt and its cited session as described in [Audit the receipt](../attested-confidential-inference.md#audit-the-receipt), then apply the provider-specific policy from that provider's verification page. `scripts/live_e2e/user_verify.py` and `scripts/live_e2e/cases/attested_sessions.py` implement this audit. Session validity and retention are described in [Attested sessions](../attested-session-system.md).

## Prefix-cache isolation

Each provider's cache-partition field, how the provider enforces it, and the
latest live observation are in [Prefix-cache isolation](../prefix-cache-isolation.md).
