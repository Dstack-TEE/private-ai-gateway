# Provider Verification

This section documents the TEE provider adapters that can produce verified upstream sessions. It is for operators setting acceptance policy and reviewers auditing what each `verified` result actually proves.

Provider pages use two document types:

- `verification.md` is a living reference tied to the current adapter and forwarding code.
- `review.md` is a dated admissions audit. It preserves the evidence and decision at that time and can contain unresolved work that has since moved.

Do not use a dated review as a substitute for the living verification page or current source.

## Provider matrix

| Provider | Attested boundary | Enforced binding | Living reference | Dated audit |
| --- | --- | --- | --- | --- |
| ACI service | ACI-compatible dstack service | `tls_spki_sha256` | [Verification](aci-service/verification.md) | First-party path; no separate audit |
| Chutes | Per-instance Intel TDX workload | `e2ee_public_key_sha256` | [Configuration](chutes/configuration.md), [verification](chutes/verification.md) | [Review](chutes/review.md) |
| NEAR AI | Intel TDX router gateway | `tls_spki_sha256` | [Verification](near-ai/verification.md) | [Review](near-ai/review.md) |
| Phala direct | Per-model dstack-vllm-proxy endpoint | `tls_spki_sha256` | [Verification](phala-direct/verification.md) | [Review](phala-direct/review.md) |
| SecretAI | SecretVM router workload | `tls_spki_sha256` | [Verification](secret-ai/verification.md) | [Review](secret-ai/review.md) |
| Tinfoil | Confidential model router | `tls_spki_sha256` | [Verification](tinfoil/verification.md) | [Review](tinfoil/review.md) |

`openai-compatible` and `anthropic` are supported transport adapters, but they do not create verified TEE sessions.

## Shared verification invariant

A provider verifier can return `verified` only with at least one enforceable channel binding. The request path then enforces the selected binding before forwarding the prompt:

- `tls_spki_sha256` pins `SHA256(SubjectPublicKeyInfo)` on the actual upstream TLS connection.
- `e2ee_public_key_sha256` restricts Chutes selection to an attested instance and encrypts the request to that instance's ML-KEM public key.

The verifier proves that the binding belongs to the attested workload according to that provider's evidence format. The forwarding backend proves that the connection or encrypted request uses the same binding. A receipt records the result and selected session.

This invariant is implemented across `src/aci/verifier/`, `src/aci/upstream/`, and `src/aggregator/service/forward.rs`. It is covered by provider bridge tests, upstream-verifier tests, and channel-binding tests.

## Claims are not uniform

`verified` means that the provider adapter's mandatory checks and binding enforcement succeeded. It does not assert every session claim.

The session layer records these claim states separately:

- TEE attestation;
- GPU attestation;
- platform TCB freshness;
- OS provenance;
- serving-software provenance;
- model-weight provenance.

Each can be asserted, refuted, or unknown. Apply relying-party policy to the claim source and evidence. In particular, do not equate a verified CPU channel with proven model weights or a CPU-bound GPU.

The common audit rubric is [Provider audit criteria](audit-criteria.md). Cross-provider router reviews are preserved in [Router-mode soundness](../reviews/router-mode-soundness.md) and [Router load balancing and cache](../reviews/router-mode-load-balancing-cache.md).

## Audit a request

For a request that must use a verified provider:

1. Set `provider.aci_verified` to `true`, or send an approved `provider.aci_session_ids` allowlist.
2. Verify the gateway report and establish its workload keyset.
3. Verify the receipt signature and exact body hashes.
4. Require an `upstream.verified` event with `required: true` and `result: verified`.
5. Resolve its `session_id` through `GET /v1/aci/sessions/{session_id}`.
6. Recompute the session identifier and evidence digest.
7. Apply the provider-specific policy described by the living verification page.

The full client flow is in [Verify an attested inference](../attested-confidential-inference.md).

## Prefix-cache isolation observation

The following is a dated operational observation, not a protocol guarantee: as observed on 2026-07-13, the gateway preserved caller-supplied `cache_salt` but did not derive a tenant-specific cache partition for the active Tinfoil and Chutes routes. Tinfoil and Chutes applied their own provider behavior after forwarding.

Revalidate provider code and deployment configuration before relying on cache partitioning. Attestation can bind a provider implementation, but it does not turn an unreviewed cache policy into tenant isolation.

## Updating a provider page

When verifier behavior changes, update the living page in the same change. Include:

- the evidence endpoints and freshness mechanism;
- every mandatory rejection check;
- the exact value bound into hardware evidence;
- how forwarding enforces that value;
- typed claims and their sources;
- evidence or checks that are supplemental only;
- current tests and a repository-relative reproduction command;
- limitations that affect a relying-party decision.

Preserve old audit results as dated records. Add a short supersession note rather than rewriting what the earlier audit observed.
