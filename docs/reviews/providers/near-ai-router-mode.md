# NEAR AI Gateway Review

Date: 2026-05-18 UTC.
Provider endpoint: `https://cloud-api.near.ai`.
Gateway source: `nearai/cloud-api`.
Reviewed gateway commit: `057135fad9e5f656baa94025d831f55391979334`.
Supporting repos:

- `nearai/nearai-cloud-verifier` at
  `8b7830e96aa4c0b2b797a2249616da7de6d0087f`
- `nearai/private-ml-sdk` at
  `25c25025c556ab2f797eeda3bab433f38a8ffb7a`
- `nearai/vllm-router` at `cfd353e`

Source reports:

- [router-mode-soundness.md](../router-mode-soundness.md)
- [router-mode-load-balancing-cache.md](../router-mode-load-balancing-cache.md)

## Verdict

NEAR AI gateway mode is acceptable as a gateway-soundness provider when the
verification lease is established through the verified gateway.

Private AI Gateway does not need to re-verify every nested `model_attestations[]` entry.
The NEAR gateway is the trust boundary. Once Private AI Gateway verifies the gateway
workload identity, source provenance, runtime policy, and TLS SPKI binding, a
model-scoped attestation response from that same verified gateway can be
treated as the gateway's statement that the requested model has attested
backend evidence.

The model evidence is still required, but its role changes:

- It is not a standalone proof that Private AI Gateway must parse and re-verify.
- It is the verified gateway's model-scoped claim that this model currently has
  an attested backend.
- It must be fetched over the verified gateway channel during lease
  establishment.

The main loophole is catalog metadata. `verifiable` and
`attestationSupported` are DB/admin-controlled fields. They are useful hints,
but they are not sufficient proof. The authoritative check for Private AI Gateway is:

```text
verified gateway channel
+ model-scoped /v1/attestation/report
+ non-empty model_attestations[]
```

## Criteria Status

Passed:

- The public gateway can expose a TLS SPKI in gateway attestation evidence.
- For vLLM/inference-url models, the reviewed gateway code verifies backend
  attestation before adding a backend to the serving pool.
- Verified backend TLS SPKIs are pinned in the gateway-to-model client.
- Empty or failed backend discovery blocks the provider instead of failing
  open.
- Lazy bucket clients re-verify and pin backend SPKI before serving traffic.
- External providers do not produce model attestation reports through the same
  path.

P0 adapter requirements:

- Verify the gateway workload identity and gateway TLS SPKI binding.
- Fetch model-scoped attestation over the verified gateway channel.
- Require non-empty `model_attestations[]` for the requested model.
- Treat catalog flags as advisory only.
- Enforce the verified gateway SPKI on every request.
- Record compact provider claims in receipts:
  `gateway_verified`, `model_evidence_present`,
  `model_attestation_count`, and a digest of the model evidence.

P0 TODOs before strict-release inclusion:

- Implement and pin the accepted gateway source/image/compose identity for the
  deployed release. This is still a code-provenance gap in Private AI Gateway's current
  NEAR verifier path, not just a documentation item.
- Define NEAR's release publication process for ACI consumers: new gateway
  source/release material and expected measurements should be available before
  production rollout, otherwise strict verifiers must fail closed or blindly
  trust an unreviewed gateway.
- Confirm the production TLS endpoint is terminated by the attested gateway,
  with no off-TEE TLS terminator.
- Confirm production runtime policy includes the intended backend image or
  compose allowlist, or document the provider-owned equivalent.
- Finish plaintext egress review for persistence, logs, traces, billing, file
  handling, and tool paths.

P1 TODOs:

- Add a negative test for external-provider models marked with misleading
  catalog flags.
- Add a live cache/locality observability test if NEAR exposes enough metadata.

## Lease Contract

The NEAR provider adapter should establish one verification lease per accepted
model.

Lease establishment:

1. Fetch the NEAR gateway attestation with `include_tls_fingerprint=true`.
2. Verify the gateway TEE quote, report-data binding, source provenance,
   runtime policy, and accepted image/compose identity.
3. Verify that the public TLS endpoint serves the SPKI bound in the gateway
   quote.
4. Pin that gateway SPKI for the lease window.
5. Fetch
   `/v1/attestation/report?model=<canonical-model>&include_tls_fingerprint=true`
   over the pinned gateway channel.
6. Require HTTP 200 and at least one `model_attestations[]` entry.
7. Record the model-attestation digest and compact gateway claim in the lease
   and receipt.

Request forwarding:

1. Select a valid lease for `(near-ai, canonical-model)`.
2. Enforce the pinned gateway TLS SPKI on the outbound HTTPS connection.
3. Forward the request normally.
4. Record the lease id / evidence digest in the ACI receipt.

Private AI Gateway Rust core should not contain a NEAR model-attestation parser. The NEAR
provider verifier owns the provider-specific lease establishment rules. Rust
only consumes the verified lease and enforces the channel binding.

## Data-Plane Findings

For `provider_type = 'vllm'` models with an `inference_url`, the NEAR gateway
has a credible verified-backend data plane.

Positive evidence:

- `load_inference_url_models` creates vLLM providers from active DB models
  with non-null `inference_url` and `provider_type != 'external'`.
- Initial discovery calls `/v1/attestation/report` on the backend with a fresh
  nonce and `include_tls_fingerprint=true`.
- `AttestationVerifier::verify_attestation_report` verifies TDX quote
  collateral, rejects TDX debug mode, verifies report-data binding, replays
  RTMR3 event-log data, extracts `os_image_hash` and `compose_hash`, and
  verifies NVIDIA evidence when present.
- After successful backend verification, the gateway pins the verified backend
  TLS SPKI in a custom Rustls verifier.
- If initial discovery produces no verified SPKI, the provider is put in
  `Blocked` state and TLS verification fails closed.
- Lazy bucket clients re-run backend verification before serving a bucket.
  Reconnects must match the pinned backend SPKI.
- The gateway does not intentionally fail open from an unverified backend.

Important source references:

- `crates/services/src/inference_provider_pool/mod.rs`
  - `load_inference_url_models`
  - `discover_model`
  - `PoolBackendVerifier::create_verified_client`
  - provider `Blocked` behavior
- `crates/inference_providers/src/spki_verifier.rs`
- `crates/inference_providers/src/vllm/mod.rs`
- `crates/services/src/attestation/verification.rs`

## Catalog and External-Provider Findings

NEAR's gateway also supports non-TEE external providers in the same codebase:
OpenAI-compatible providers, Anthropic, and Gemini.

This is not a data-plane bypass for verified vLLM models, but it is a catalog
loophole if Private AI Gateway trusts metadata flags alone.

Concrete risks:

- `/v1/model/list` returns `verifiable`, `attestationSupported`, and
  `providerType` from model metadata. These fields are admin/DB state, not
  derived from the live provider pool.
- An external model can be marked incorrectly in catalog metadata. It still
  cannot produce a model attestation through the NEAR attestation path, but a
  client that trusted flags alone could be misled.
- A model's `inference_url` can change through admin/catalog state. The gateway
  refresh path should re-run discovery and block until attestation succeeds,
  but Private AI Gateway must not cache a model lease beyond its verification TTL.

Adapter rule:

```text
/v1/model/list flags are advisory.
The model-scoped attestation report over the verified gateway channel is
authoritative for lease establishment.
```

## `model_attestations[]` Semantics

The attestation endpoint returns `model_attestations[]` by fetching backend
attestation evidence through the gateway's verified/pinned backend path. The
gateway does not sign the nested entries or bind them into the gateway quote.

So the correct meaning is:

```text
The verified NEAR gateway asserts that this model currently has backend
attestation evidence, and it returned that evidence over a verified gateway
TLS channel.
```

That is enough for gateway-soundness mode if Private AI Gateway has accepted the gateway
as the trust boundary. The nested entries are useful audit artifacts and should
be hashed or recorded, but Private AI Gateway does not need to re-verify them during lease
establishment.

## Required Gateway Assumptions

Gateway-soundness mode is acceptable only when these assumptions are verified
or pinned:

- The gateway attestation maps to the audited `nearai/cloud-api` source or a
  release we accept.
- The gateway TLS certificate is served by the attested workload. There must
  be no off-TEE TLS terminator that breaks the SPKI binding.
- The accepted runtime policy includes the intended upstream verifier behavior.
  In particular, `ALLOWED_IMAGE_HASHES` should be configured or the deployment
  should otherwise prove the allowed backend image/compose set.
- The lease is refreshed often enough that model catalog changes, provider
  refreshes, and backend key rotation do not outlive Private AI Gateway's cached trust.
- Private AI Gateway treats external providers as not verified unless the verified gateway
  can produce model-scoped attestation evidence for that model.

## Privacy and Operations Caveats

The review found feature surfaces that matter to the trust boundary:

- Some response/conversation paths persist content as JSONB. This does not
  prove ordinary chat-completions writes every request body, but persistence
  features are part of the privacy review.
- Debug logging can include streaming event payloads if production logging is
  configured too verbosely.
- Observability, billing, file handling, tool execution, and auto-redaction
  paths must stay inside the accepted gateway trust boundary or be proven not
  to carry sensitive plaintext.

These caveats do not require Private AI Gateway to re-verify nested model attestations.
They are gateway provenance/config requirements.

## Load Balancing and Cache Findings

NEAR has a real prefix-aware routing implementation, but the result is not
externally observable today.

Architecture:

1. `cloud-api.near.ai` runs the outer gateway in a dstack TEE.
2. The gateway uses a `PrefixRouter` to map message prefixes to buckets.
3. Each bucket owns a pinned H2 client.
4. An upstream L4 passthrough load balancer chooses the concrete vLLM-proxy
   backend when the bucket's connection is created.
5. The model CVM may also run `nearai/vllm-router` internally, but that inner
   router is part of the model workload and is not separately visible to
   Private AI Gateway.

PrefixRouter behavior:

- Source: `crates/inference_providers/src/vllm/prefix_router.rs`.
- The router is message-level, not token-level. Each trie edge is a hash of one
  chat message.
- For each message, the hash includes a role tag
  (`system=0`, `user=1`, `assistant=2`, `tool=3`) and message content.
- String content is hashed directly. Array content hashes only each part's
  `text` field. Other JSON content is hashed by its string representation.
- Only the first `PREFIX_MAX_MESSAGES` messages are considered. The default is
  8 messages.
- Empty message lists route to bucket 0.
- A new first-message prefix gets a fresh bucket id:
  `next_bucket % NUM_PREFIX_BUCKETS`.
- `NUM_PREFIX_BUCKETS` defaults to 64 and is capped at 1024.
- Deeper trie nodes inherit the parent bucket. Requests with the same first
  message, usually the system prompt, stay on the same bucket even when later
  user messages differ.

Bucket and H2-client behavior:

- Source: `crates/inference_providers/src/vllm/mod.rs`.
- Each prefix bucket owns a long-lived HTTP/2 client.
- A long-lived HTTP/2 client usually means one persistent TCP/TLS/H2
  connection.
- With an upstream L4 passthrough load balancer, the concrete backend is chosen
  when that connection is established.
- Keeping the same bucket client alive therefore tends to keep later matching
  prefixes on the same verified backend.
- The intended chain is:
  `same prefix -> same bucket -> same H2 connection -> same backend -> better
  KV-cache locality`.
- This is a locality mechanism, not a cryptographic proof of cache state.

Inner `nearai/vllm-router` behavior:

- Source: `prefix/hashtrie.py` and `routers/routing_logic.py` in
  `nearai/vllm-router`.
- This is a separate router that may run inside the model CVM. It is not the
  public `cloud-api` gateway PrefixRouter.
- It flattens chat messages into a prompt string by concatenating message text
  content with newlines.
- It chunks the prompt into 128-character chunks, hashes each chunk with
  `xxhash64`, and performs longest-prefix match over a trie.
- It selects an endpoint from the longest matching endpoint set, then inserts
  the selected endpoint for the prompt.
- Its own comments say it assumes no prefix-cache eviction, so it is also a
  best-effort routing hint rather than a verifiable cache-hit guarantee.

Cache locality:

- Code indicates cache-aware routing for stable message prefixes.
- We cannot independently verify cache locality from current public responses.
- Live responses expose `inference-id`, but no backend id, bucket id,
  fallback flag, or cached-token counter.
- Multiple `cloud-api` gateway replicas likely have independent PrefixRouter
  state; cache locality is per gateway replica, not global.

## Live Evidence

Live probes during review:

- `GET https://cloud-api.near.ai/v1/attestation/report?signing_algo=ecdsa`
  returned a gateway-only attestation with empty `model_attestations[]`.
- The same endpoint with
  `model=google/gemma-4-31B-it&include_tls_fingerprint=true` returned one
  model attestation entry with `intel_quote`, `nvidia_payload`, `event_log`,
  `tls_cert_fingerprint`, `ohttp_attestation`, and
  `compose_manager_attestation`.
- Authenticated chat completion returned `inference-id` but no backend id,
  bucket id, or cache-hit metadata.
- `/v1/signature/<bogus-chat-id>` returned a chat-id keyed not-found shape,
  confirming the signature lookup path exists.
- Existing aggregator live artifact:
  `/tmp/private-ai-gateway-live-e2e/20260518-053819`.

## Required Adapter Changes

P0:

- Move NEAR verification to gateway-soundness lease establishment:
  verify gateway identity/provenance/TLS binding, then require a model-scoped
  attestation report over the verified gateway channel.
- Stop treating nested `model_attestations[]` verification as a Rust
  aggregator responsibility. The provider verifier may record the nested
  evidence digest, but Rust should enforce only the verified gateway lease and
  channel binding.
- Treat `/v1/model/list` catalog flags as hints. They may help choose models to
  probe, but they must not authorize a lease without a successful model-scoped
  attestation response.
- Reject or do not configure model IDs that cannot produce non-empty
  `model_attestations[]` through the verified gateway.

P1:

- Surface and pin the gateway compose/image digest in the NEAR verifier result.
  The allowlist should map audited source/release evidence to accepted
  deployment digests.
- Record `model_attestations_sha256` and model canonical id in the lease and
  receipt.
- Decide how to represent the gateway runtime-policy assumptions, especially
  upstream image/compose allowlists, without adding a generic policy DSL.
- Ask NEAR to expose stable bucket/backend/cache-hit evidence, for example
  `X-NearAI-Bucket`, `X-NearAI-Backend`, and cache-hit metadata.

P2:

- Add a live external-provider sentinel test: choose a known external model if
  one is public, verify `model_attestations[]` is empty, and assert the
  aggregator refuses to forward it in verified mode.
- Add an opt-in prefix-cache locality probe using a large stable prompt prefix
  and a short changing suffix. Treat timing-only evidence as weak unless NEAR
  exposes backend or cache metadata.
- Track whether live NEAR enables strict image-hash and TCB freshness policy,
  then fold those expectations into the verifier bridge.

## Open Questions

- Can we run NEAR's `nearai-cloud-verifier` end to end against the live gateway
  report and recover the exact source commit/image digest?
- Which public NEAR models are guaranteed to be `inference_url` TEE-backed
  models, and can this be listed or proven without trial requests?
- Does ordinary chat-completions without conversation state write request or
  response bodies to the JSONB persistence path?
- Can NEAR expose a gateway-owned trusted model catalog claim so Private AI Gateway can
  avoid probing every model individually?
