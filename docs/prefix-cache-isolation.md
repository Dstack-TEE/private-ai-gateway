# Prefix-cache isolation

Inference engines reuse the key-value cache of a prompt prefix they have
already seen, so a shared cache can reveal to one tenant that another tenant
sent the same prefix. This page states how the gateway partitions provider
prompt caches per user, what each provider does with the partition field, and
what has been observed in live tests.

Middleware mode automatically partitions supported prompt caches by the
control plane's authenticated `userId`.
The gateway removes caller `cache_salt` and `user_cache_secret` fields before
request transforms, including native Responses and Messages passthrough. A
non-empty string `cache_salt` subdivides the authenticated user's partition;
it cannot opt into another user's cache. Other JSON values and empty strings
are ignored. Anonymous requests receive no generated salt. Direct mode
(without middleware) retains its existing caller-field behavior.

## Provider support

| Provider | Middleware field | Limitation |
| --- | --- | --- |
| Chutes | `cache_salt` | Passthrough to vLLM is an observed provider behavior, not an attested cache-isolation guarantee. It was observed on the gateway's end-to-end-encrypted instance path; the public `llm.chutes.ai` API ignored `cache_salt` when called directly. |
| Privatemode | `cache_salt` | The proxy uses the effective salt for cache/shard affinity; see [Prompt cache](providers/privatemode/verification.md#prompt-cache). |
| Tinfoil | `user_cache_secret` | Requires the provider router's `CACHE_SALT_ENABLED` policy. The router combines this secret with the shared upstream credential to derive its own salt. |
| Phala Direct | `cache_salt` | The production chain (PIG → vllm-router → SGLang or vLLM) forwards the field; the engine enforces isolation. With an SGLang storage tier (HiCache/Mooncake), require SGLang ≥ 0.5.20, whose storage keys include the salt; 0.5.19 partitions only GPU cache. |
| NEAR AI | `cache_salt` | Honored by NEAR's TEE vLLM models. |
| ArmetAI | `cache_salt` | Isolation was last verified on vLLM. Enforcement on the current SGLang deployment remains untested. |
| 0G Private Computer | `cache_salt` | The adapter seals the field. Broker source for `v0.6.4` and `v0.6.5` omits it from the top-level allowlist for self-hosted engines, so the broker may drop it. Forwarding and isolation remain unverified live for those versions. |
| Other providers | None | The gateway injects no partition field. |

## Which requests carry the field

Injection applies only when the resolved upstream path ends in
`/chat/completions`, `/completions`, `/responses`, or `/messages`, including
buffered, streamed, and failover attempts. Audio forms, embeddings, image
generation, and System One do not receive a generated cache field. Unsupported
routes and anonymous middleware requests still lose both caller fields.

## Salt derivation and limits

The gateway uses a separate TEE-held dstack KMS key to derive provider-specific
salts, keeps salts out of logs and control reports, and covers forwarded bytes
only through receipt hashes. Isolation assumes an honest control plane and a
provider that honors the field; stable values remain linkable within one
provider, and attestation alone does not prove cache enforcement. See
[Cache partition derivation and rotation](configuration-reference.md#cache-partition-derivation-and-rotation)
for the exact construction and lifecycle.

## Observed enforcement

As observed on 2026-11-12 on disposable CVMs (last at commit `99e28f1d`),
Tinfoil (`gpt-oss-120b`), Chutes (`GLM-5.1-TEE`, `Kimi-K2.6-TEE`),
Privatemode (`glm-5.3-flash`), NEAR AI (`glm-5.3-flash`,
`Qwen3.6-35B-A3B-FP8`), ArmetAI (`GLM-5.3-Flash`) and Phala Direct
(`qwen3.8-27b`, `glm-5.3`) behaved as intended for buffered and streamed chat:
a tenant's repeated prompt hit the cache, while another tenant, the same
caller salt under another tenant, and an anonymous request missed in every
trial, and a restart kept the partition. A direct-mode probe on the same
day showed 0G Private Computer (`glm-5.3`, `0GM-1.0-35B-A3B`) honoring a sealed
`cache_salt` the same way.

The ArmetAI observation predates its move to SGLang. The 0G observation
predates broker `v0.6.x`. Production rollout remains unverified.
[Test per-user cache salts on a disposable CVM](cache-salt-cvm-test.md)
reproduces the check.
