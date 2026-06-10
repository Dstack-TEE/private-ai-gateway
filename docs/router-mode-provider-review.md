# Router-Mode Provider Review

Status: first-pass review complete. Provider-specific reports and the shared
admission checklist are under `docs/providers/`.

Router-mode verification is acceptable only when the router is itself a
trusted workload and the router-to-model path preserves the same privacy and
integrity guarantees. Verifying a router's TEE quote is not enough by itself.

## Scope

Initial providers:

- Tinfoil `inference.tinfoil.sh` and its confidential model router.
- NEAR AI `cloud-api.near.ai` gateway mode.

Out of scope for this review:

- Direct model endpoints where the client connects straight to the model CVM.
- Chutes E2EE transport.
- Private AI Gateway's current internal model router, except as a comparison point.

## Acceptance Questions

Soundness and privacy:

1. What exact router source, config, image, and measurement are bound to the
   attestation?
2. Does the verified source contain any path that can expose plaintext prompts,
   completions, API keys, files, tool payloads, or embeddings outside a TEE?
3. Are debug/local bypasses impossible in production measurements?
4. Does the router verify every downstream model CVM before adding it to the
   serving pool?
5. Does the router-to-model transport enforce an attested TLS, HPKE, E2EE, or
   equivalent key for the selected downstream CVM?
6. Are billing, usage extraction, metrics, logs, tool execution, file upload,
   retries, and error handling inside the reviewed trust boundary or proven not
   to carry sensitive plaintext?

Load balancing and cache locality:

1. How does the router choose among multiple model CVMs?
2. Does it observe per-instance health, queue depth, overload, and circuit
   breaker state?
3. Does it retry or reroute after a partial request has been sent?
4. Does it preserve session, prompt-cache, or KV-cache locality when the
   provider exposes a cache/session key?
5. If no cache-affinity mechanism exists, can cached-token metadata or backend
   instance headers prove whether cache behavior survives router load
   balancing?
6. What live traffic pattern would reveal broken cache locality or unstable
   routing?

## Review Process

Each reviewer should clone the provider router/gateway source into a temporary
directory and record:

- Repository URL.
- Commit hash.
- Any release tag or image digest tied to public attestation evidence.
- Commands run.
- Public docs consulted.
- Live endpoints tested, with timestamps and artifact paths.

For each docs claim, cite the code path or mark it unverified. For each code
path carrying sensitive material, identify whether it remains inside the
attested workload and whether it can leave through logs, metrics, billing
events, external tools, or non-attested upstream calls.

Do not treat provider claims as proof. The review conclusion must be based on
independently checked source, attestation evidence, and live behavior when live
testing is practical.

## Required Outputs

Write separate review notes under `docs/reviews/`:

- `reviews/router-mode-soundness.md`
- `reviews/router-mode-load-balancing-cache.md`
- `providers/tinfoil/review.md`
- `providers/near-ai/review.md`

Each note must include:

- Summary conclusion: acceptable, acceptable with conditions, or not acceptable.
- Evidence table: claim, evidence, source/code reference, confidence.
- Concrete risks and open questions.
- Live or unit tests performed.
- Recommended changes to our gateway/provider adapter.

## Current Conclusions

Tinfoil router mode is acceptable with conditions: the reviewed router verifies
downstream model enclaves and forwards over attested TLS-bound transports, but
strict release mode must pin the audited router compose/image identity and
decide the runtime config update policy.

NEAR AI gateway mode is acceptable with conditions: the gateway can be the
trust boundary when Private AI Gateway verifies the gateway workload, enforces the
gateway TLS SPKI, and fetches model-scoped attestation evidence over that
verified channel. Private AI Gateway treats catalog flags as advisory and does not
re-verify nested model attestations in the normal lease path.

For both providers, cache-aware routing is limited or unproven. Tinfoil should
not claim cache locality. NEAR has prefix-aware routing internally, but it is
not a cryptographic or externally observable cache guarantee today.
