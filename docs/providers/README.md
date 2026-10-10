# Provider Verification

This section documents the TEE provider adapters that can produce verified upstream sessions. It is for operators setting acceptance policy and reviewers auditing what each `verified` result actually proves.

Each provider has a living `verification.md` page for the current adapter and, once audited, a dated `review.md` admission record; see [Documentation conventions](../README.md#documentation-conventions). A verification page states the evidence endpoints and freshness mechanism, the mandatory rejection checks, the value bound into hardware evidence and how forwarding enforces it, the typed session claims and their sources, supplemental evidence, tests, and limitations. A verifier change updates its verification page in the same change, as described in [Contributing](../../CONTRIBUTING.md#provider-verification).

## Provider matrix

| Provider | Attested boundary | Enforced binding | Living reference | Dated audit decision |
| --- | --- | --- | --- | --- |
| ACI service | ACI-compatible dstack service | `tls_spki_sha256` | [Verification](aci-service/verification.md) | First-party path; no separate audit |
| Chutes | Per-instance Intel TDX workload | `e2ee_public_key_sha256` | [Configuration](chutes/configuration.md), [verification](chutes/verification.md) | [Accepted for limited traffic](chutes/review.md), 2026-05-18 |
| NEAR AI | Intel TDX router gateway | `tls_spki_sha256` | [Verification](near-ai/verification.md) | [Acceptable with conditions](near-ai/review.md), 2026-05-18 |
| Phala direct | Per-model dstack-vllm-proxy endpoint | `tls_spki_sha256` | [Verification](phala-direct/verification.md) | [Acceptable with conditions](phala-direct/review.md), 2026-06-10 |
| Privatemode | Official proxy co-deployed in the gateway's measured Compose | `proxy_image_sha256` | [Verification](privatemode/verification.md) | [Acceptable with conditions](privatemode/review.md), 2026-05-26 |
| SecretAI | SecretVM router workload | `tls_spki_sha256` | [Verification](secret-ai/verification.md) | [Acceptable with conditions](secret-ai/review.md), 2026-05-22 |
| Tinfoil | Confidential model router | `tls_spki_sha256` | [Verification](tinfoil/verification.md) | [Acceptable with conditions](tinfoil/review.md), 2026-05-18 |

`openai-compatible` and `anthropic` are supported transport adapters, but they do not create verified TEE sessions.

## What `verified` means

A provider verifier returns `verified` only with at least one enforceable channel binding, and the gateway forwards a prompt only over a connection that enforces that binding; [Request-time flow](../upstream-verification-lifecycle.md#request-time-flow) describes the steps. The check lives in `src/aci/verifier/`, `src/aci/upstream/`, and `src/aggregator/service/forward.rs`, and `tests/upstream_verifier.rs::service_fails_if_selected_backend_cannot_enforce_channel_binding` covers the fail-closed case.

`verified` does not assert every typed session claim. Each verification page lists the claims its adapter asserts, and [Attested sessions](../attested-session-system.md#how-claims-are-derived) explains how claims and their sources are derived.

The common audit rubric is [Provider audit criteria](audit-criteria.md). The cross-provider router reviews are [Router-mode soundness](../reviews/router-mode-soundness.md) and [Router load balancing and cache](../reviews/router-mode-load-balancing-cache.md), and [Router-mode provider review](../router-mode-provider-review.md) records how those reviews were run.

## Audit a request

Check the receipt and its cited session as described in [Audit the receipt](../attested-confidential-inference.md#audit-the-receipt), then apply the provider-specific policy from that provider's verification page. `scripts/live_e2e/user_verify.py` and `scripts/live_e2e/cases/attested_sessions.py` implement this audit. Session validity and retention are described in [Attested sessions](../attested-session-system.md).

## Prefix-cache isolation observation

The following is a dated operational observation, not a protocol guarantee. As
observed on 2026-07-13, the gateway preserved caller-supplied `cache_salt` but
did not derive a tenant-specific cache partition for the active Tinfoil and
Chutes routes:

- Tinfoil
  [replaced `cache_salt`](https://github.com/tinfoilsh/confidential-model-router/blob/v0.0.118/cache_salt.go)
  with a value derived from RedPill's shared upstream credential. Because the
  gateway did not set `user_cache_secret`, RedPill tenants shared one provider
  cache namespace.
- Chutes passed `cache_salt` to vLLM but did not generate one. Unsalted
  requests shared the serving instance's namespace.

At that revision, Tinfoil's behavior was attestation-backed. The observed
Chutes behavior came from control-plane evidence and was not bound by its
attestation. The intended caller-controlled interface was to preserve
`cache_salt` for Chutes and translate it to `user_cache_secret` for Tinfoil,
without deriving or overriding it from RedPill tenant identity in the gateway.

Revalidate provider code and deployment configuration before relying on cache
partitioning. Attestation can bind a provider implementation, but it does not
turn an unreviewed cache policy into tenant isolation.
