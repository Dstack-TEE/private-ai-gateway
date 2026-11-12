# Provider Verification

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

The common audit rubric is [Provider audit criteria](audit-criteria.md). The cross-provider router reviews are [Router-mode soundness](../reviews/router-mode-soundness.md) and [Router load balancing and cache](../reviews/router-mode-load-balancing-cache.md), and [Router-mode provider review](../router-mode-provider-review.md) records how those reviews were run.

## Audit a request

Check the receipt and its cited session as described in [Audit the receipt](../attested-confidential-inference.md#audit-the-receipt), then apply the provider-specific policy from that provider's verification page. `scripts/live_e2e/user_verify.py` and `scripts/live_e2e/cases/attested_sessions.py` implement this audit. Session validity and retention are described in [Attested sessions](../attested-session-system.md).

## Prefix-cache isolation observation

Middleware mode automatically partitions supported prompt caches by the
control plane's authenticated `userId`. This supersedes the caller-controlled,
cross-user sharing policy discussed in [#83](https://github.com/Dstack-TEE/private-ai-gateway/issues/83).
The gateway removes caller `cache_salt` and `user_cache_secret` fields before
request transforms, including native Responses and Messages passthrough. A
non-empty string `cache_salt` subdivides the authenticated user's partition;
it cannot opt into another user's cache. Other JSON values and empty strings
are ignored. Anonymous requests receive no generated salt. Direct mode
(without middleware) retains its existing caller-field behavior.

| Provider | Middleware field | Limitation |
| --- | --- | --- |
| Chutes | `cache_salt` | Passthrough to vLLM is an observed provider behavior, not an attested cache-isolation guarantee. It was observed on the gateway's end-to-end-encrypted instance path; the public `llm.chutes.ai` API ignored `cache_salt` when called directly. |
| Privatemode | `cache_salt` | The proxy uses the effective salt for cache/shard affinity; see [Prompt cache](privatemode/verification.md#prompt-cache). |
| Tinfoil | `user_cache_secret` | Requires the provider router's `CACHE_SALT_ENABLED` policy. The router combines this secret with the shared upstream credential to derive its own salt. |
| Phala Direct | `cache_salt` | The production chain (PIG → vllm-router → SGLang or vLLM) forwards the field; the engine enforces isolation. With an SGLang storage tier (HiCache/Mooncake), require SGLang ≥ 0.5.20, whose storage keys include the salt; 0.5.19 partitions only GPU cache. |
| NEAR AI | `cache_salt` | Honored by NEAR's TEE vLLM models. |
| ArmetAI | `cache_salt` | Enforced by vLLM in the measured TD. |
| Other providers | None | Secret AI uses Ollama, which has no partition field; 0G's engine is not established. Non-TEE providers receive no field. |

Injection applies only when the resolved upstream path ends in
`/chat/completions`, `/completions`, `/responses`, or `/messages`, including
buffered, streamed, and failover attempts. Audio forms, embeddings, image
generation, and System One do not receive a generated cache field. Unsupported
routes and anonymous middleware requests still lose both caller fields.

The gateway uses a separate TEE-held dstack KMS key to derive provider-specific
salts, keeps salts out of logs and control reports, and covers forwarded bytes
only through receipt hashes. Isolation assumes an honest control plane and a
provider that honors the field; stable values remain linkable within one
provider, and attestation alone does not prove cache enforcement. See
[Cache partition derivation and rotation](../configuration-reference.md#cache-partition-derivation-and-rotation)
for the exact construction and lifecycle.

As observed on 2026-11-12 on disposable CVMs (last at commit `99e28f1d`),
Tinfoil (`gpt-oss-120b`), Chutes (`GLM-5.1-TEE`, `Kimi-K2.6-TEE`),
Privatemode (`glm-5.3-flash`), NEAR AI (`glm-5.3-flash`,
`Qwen3.6-35B-A3B-FP8`), ArmetAI (`GLM-5.3-Flash`) and Phala Direct
(`qwen3.8-27b`, `glm-5.3`) behaved as intended for buffered and streamed chat:
a tenant's repeated prompt hit the cache, while another tenant, the same
caller salt under another tenant, and an anonymous request missed in every
trial, and a restart kept the partition. Production rollout remains
unverified.
[Test per-user cache salts on a disposable CVM](../cache-salt-cvm-test.md)
reproduces the check.
