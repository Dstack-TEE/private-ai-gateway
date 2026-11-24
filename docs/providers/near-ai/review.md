# NEAR AI gateway review

Date: 2026-11-23 PST (round 2). Original review 2026-05-18 UTC. Verdict and
lease contract updated 2026-06-15 for router-scoped sessions.

> [!NOTE]
> This is a dated admission record. [Round 2](#round-2-2026-11-23-pst)
> reviews the production release `prod-20261119-a61bf3b` and supersedes the
> earlier verdict and workload claims. Source paths reflect each round's
> reviewed release. See [verification.md](verification.md) for current adapter
> behavior.

## Round 2 (2026-11-23 PST)

Reviewed the deployed NEAR AI gateway release
[`prod-20261119-a61bf3b`](https://github.com/nearai/cloud-api/releases/tag/prod-20261119-a61bf3b).
It corresponds to `nearai/cloud-api` commit
`a61bf3b44498d1447f3e39e1d3b84a18dbc5a568`, 429 commits after round 1, and
image
`nearaidev/cloud-api@sha256:d996bb08f36f0cc913ed7763f7c42bf4124e386a679e397a9acd8a3e69f3a704`.
The supporting source revisions were `nearai/nearai-cloud-verifier`
`94554726` and `nearai/cvm-ingress` `5654d298`. The round-1
`nearai/private-ml-sdk` and `nearai/vllm-router` revisions are unchanged and
both repositories are archived.

### Verdict

**Acceptable with conditions** for the gateway channel (router-scoped
attestation).

**Not acceptable** for model-scoped verified-inference claims: findings 1
and 2 fall under the [hard reject conditions](../audit-criteria.md#hard-reject-conditions)
for unverified model paths and alias selection. Admission for verified model
inference is withheld until backend workload admission and model-identity
matching are enforced.

- Configure canonical model IDs from `/v1/model/list`. NEAR administrators can
  change which model an alias resolves to.
- Use NEAR-hosted models (`providerType: vllm`). This verdict excludes
  third-party providers routed through NEAR.

### Live Check (2026-11-23 20:07–20:12 PST)

- `/v1/attestation/report` returned HTTP 200 without credentials when called
  with a fresh nonce and `include_tls_fingerprint=true`. The quote's
  `report_data` bound the nonce and SHA-256 of the concatenated signing
  address and TLS SPKI fingerprint. The live TLS endpoint served the attested
  SPKI fingerprint
  `73862c254372091cc6895b900111503860694f8916ebf0c63b18d245ba8ba5ad`.
- The gateway reported app ID `f550fdfb4eb8ad787c1bcd423f091cbb4a4431ae`,
  compose hash `1787a4af87eaf4bb139b555c3148ea6cffb01442b9fd7db4304e23b24f4db993`,
  and OS `dstack-0.5.12` with hash
  `8db40cb23008d6213668b5816702d100b37b859a8a5bfd18d938ae3a1b3715bd`. The
  measured compose hash matched the RTMR3 event, and the compose pinned the
  release image digest. GitHub build provenance verified for that digest and
  linked it to `nearai/cloud-api` at `a61bf3b4`.
- At Base mainnet block `61515511`, the app contract allowed the measured
  gateway compose hash and returned `true` for `allowAnyDevice()`.
- With dstack-verifier 0.6.1, the adapter returned `verified` with TCB status
  `UpToDate` for `Qwen/Qwen3.8-27B` and
  `black-forest-labs/FLUX.2-klein-4B`. The probed Chutes, Tinfoil, and
  external models failed lease establishment. The retired
  `google/gemma-4-31B-it` returned HTTP 400.
- Catalog flags remain advisory. The external model `levanto/sage-2` was
  marked `verifiable: true` and its attestation endpoint returned HTTP 503.

### Resolved or Improved

- The measured compose runs `cvm-ingress` in the gateway CVM. The reviewed
  ingress source configures nginx to terminate TLS there and encrypts the
  certificate archive with a dstack KMS-derived key before storing it in S3.
- The measured gateway compose, pinned image digest, GitHub build provenance,
  and on-chain compose allowlist provide evidence for a gateway workload pin.
  The adapter still lacks that pin. See
  [verification.md](verification.md#limitations).

### Open Findings

1. **Production has no backend image or compose allowlist.** The NEAR-hosted
   [measurement policy](https://github.com/nearai/cloud-api/blob/a61bf3b44498d1447f3e39e1d3b84a18dbc5a568/crates/services/src/attestation/measurement.rs)
   skips the OS-image hash check when `ALLOWED_IMAGE_HASHES` is empty.
   `REQUIRE_TCB_UP_TO_DATE` controls whether a stale TCB fails verification
   instead of raising a warning. Neither setting is present in the measured
   production compose or its allowed environment variables. The source check
   found no separate backend compose or app ID allowlist. For workload
   identity, any genuine non-debug dstack TDX CVM can pass the backend
   attestation checks with valid report-data binding. This blocks admission
   for verified model inference under the hard reject condition for
   unverified model paths.
2. **Administrators can retarget model aliases.** The attestation report
   requested for `zai-org/GLM-5.1-FP8` named `z-ai/glm-5.3-flash`. The
   adapter still returned `verified` for the alias. This blocks admission
   for verified model inference under the hard reject condition for silent
   alias selection.
3. **Third-party routes are outside the reviewed scope.** Chutes and Tinfoil
   models can be marked verifiable in the catalog. Lease establishment failed
   for the probed models.
4. **Gateway releases are published at promotion.** Pre-rollout publication
   remains open under criterion 7.
5. **Telemetry and persistence cross the gateway boundary.** The measured
   compose exports container logs to a remote collector and the gateway
   uses an external database. Which request or response data reaches either
   destination remains unverified.

### Not Re-verified

- Authenticated inference, response model identity under an alias, signature
  lookup, cache metadata, and request fidelity were outside the live check.
- The prior live tamper checks were not repeated. This round recomputed the
  nonce and TLS binding.
- Backend TLS pinning, failure handling, and routing were not re-audited at
  the deployed revision. The source search for backend allowlists covered the
  measurement policy and relevant references, with the full provider pool
  implementation outside the review. The round-1 data-plane and cache
  sections describe `057135fa`. The old `vllm` module paths moved into
  `crates/inference_providers/src/attested/`.
- The ingress and telemetry image digests were not traced to source. Gateway
  and backend OS images were not traced to reproducible builds. The
  on-chain allowlist history, owner key custody, and KMS root remain unreviewed.

## Round 1 (2026-05-18 UTC)

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

- [router-mode-soundness.md](../../reviews/router-mode-soundness.md)
- [router-mode-load-balancing-cache.md](../../reviews/router-mode-load-balancing-cache.md)

> **Status (2026-06 soundness pass):** the gateway now parses `report_data` from the
> verified quote and enforces the nonce + signing-address + TLS-SPKI binding (commit
> `ca7ddbd`), addressing the P0 "verify gateway identity/provenance/TLS binding" item
> below. The strict gateway compose/image digest pin remains a follow-up.

### Verdict

NEAR AI gateway mode is acceptable as a gateway-soundness provider when the
verification lease is established through the verified gateway.

NEAR AI is a router (`AttestationScope::PerRouter`): the NEAR gateway is the
trust boundary, and the attested session is that one verified gateway channel,
shared by every model behind it. Private AI Gateway verifies the gateway
workload identity, source provenance, runtime policy, and TLS SPKI binding — and
nothing about the served model. It does **not** fetch, parse, or check the
nested `model_attestations[]`: the gateway does not re-verify those quotes and
they are not bound to the instance that serves a given request, so they cannot
stand as a sound per-model attestation. The served model is recorded on the
receipt as an identifier; a request-bound, per-instance model attestation is a
roadmap item.

The main loophole is catalog metadata. `verifiable` and
`attestationSupported` are DB/admin-controlled fields. They are useful hints,
but they are not sufficient proof. The authoritative check for Private AI Gateway is:

```text
verified gateway channel
(workload identity + source provenance + runtime policy + TLS SPKI binding)
```

### Criteria Status

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
- Treat catalog flags as advisory only.
- Enforce the verified gateway SPKI on every request.
- Record compact, channel-scoped provider claims in receipts (e.g.
  `gateway_verified`, `gateway_tls_spki_sha256`, `tcb_status`). The attested
  session is the gateway channel; per-model TEE coverage is delegated to the
  verified gateway, with the served model recorded as a receipt-level identifier.

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

### Lease Contract

The NEAR provider adapter establishes one verification lease per gateway channel
(router scope): every accepted model behind the gateway shares it.

Lease establishment:

1. Fetch the NEAR gateway attestation with `include_tls_fingerprint=true`.
2. Verify the gateway TEE quote, report-data binding, source provenance,
   runtime policy, and accepted image/compose identity.
3. Verify that the public TLS endpoint serves the SPKI bound in the gateway
   quote.
4. Pin that gateway SPKI for the lease window.
5. Fetch
   `/v1/attestation/report?model=<canonical-model>&include_tls_fingerprint=true`
   over the pinned gateway channel. The model is only the shape of NEAR AI's
   endpoint; the gateway attestation it returns is the same for every model.
6. Require HTTP 200 and a verified gateway component. The nested
   `model_attestations[]` are neither required nor checked (see
   [`model_attestations[]` Semantics](#model_attestations-semantics)).
7. Record the channel-scoped gateway claim in the lease and receipt; the served
   model is a receipt-level identifier only.

Request forwarding:

1. Select a valid lease for `(near-ai, canonical-model)`.
2. Enforce the pinned gateway TLS SPKI on the outbound HTTPS connection.
3. Forward the request normally.
4. Record the lease id / evidence digest in the ACI receipt.

Private AI Gateway Rust core should not contain a NEAR model-attestation parser. The NEAR
provider verifier owns the provider-specific lease establishment rules. Rust
only consumes the verified lease and enforces the channel binding.

### Data-Plane Findings

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

### Catalog and External-Provider Findings

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
The verified gateway channel is authoritative for lease establishment.
```

### `model_attestations[]` Semantics

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
as the trust boundary. Private AI Gateway does not consume the nested entries: they are
not bound to the instance that serves a given request, so they cannot stand as a
sound per-model attestation and are not recorded in the attested session. A
future request-bound, per-instance model attestation would surface them on the
receipt — not in the channel session.

### Required Gateway Assumptions

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
- Per-model TEE coverage is delegated to the verified gateway: the gateway
  verifies its backend model TDs and only serves verified backends, and we verify
  the gateway's own integrity and source provenance — so the gateway is trusted
  because it is itself verifiable, not blindly. The served model is recorded on
  the receipt as an identifier; binding the exact backend instance to a specific
  request is a roadmap refinement.

### Privacy and Operations Caveats

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

### Load Balancing and Cache Findings

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

### Live Evidence

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

### Adapter Behavior

- NEAR verification is a gateway-soundness lease: the gateway identity, source
  provenance, runtime policy, and TLS SPKI binding are verified. NEAR AI is a
  router, so the verified gateway channel is the attested session; the
  model-scoped report is only the shape of NEAR's endpoint.
- Per-model TEE coverage is delegated to the verified gateway, which verifies its
  backend model TDs and only serves verified backends; because the gateway is
  itself verified for integrity and provenance, that delegation is sound. The
  nested `model_attestations[]` are not re-verified or recorded here, and the
  served model is a receipt-level identifier.
- `/v1/model/list` catalog flags are never trusted; the lease is authorized by
  the verified gateway channel.

### Remaining

- Surface and pin the gateway compose/image digest in the NEAR verifier result,
  mapping audited source/release evidence to accepted deployment digests.
- Bind the exact backend instance to a specific request (a per-instance,
  request-bound model attestation on the receipt), tightening today's delegation
  to the gateway.
- Decide how to represent the gateway runtime-policy assumptions, especially
  upstream image/compose allowlists, without adding a generic policy DSL.
- Ask NEAR to expose stable bucket/backend/cache-hit evidence, for example
  `X-NearAI-Bucket`, `X-NearAI-Backend`, and cache-hit metadata.
- Add a sentinel test that the verified gateway refuses to serve a known
  external (non-TEE) model, confirming the delegation assumption holds.
- Add an opt-in prefix-cache locality probe (large stable prompt prefix + short
  changing suffix); treat timing-only evidence as weak unless NEAR exposes
  backend or cache metadata.
- Track whether live NEAR enables strict image-hash and TCB freshness policy,
  then fold those expectations into the verifier bridge.

### Source & platform provenance, and TCB status

Tracking criteria 13–14 of [audit-criteria.md](../audit-criteria.md):

- **Software provenance** (gateway/model code → reviewed source): partial —
  `SHA256(app_compose) == reported compose_hash` is self-consistent only, not pinned to
  a reviewed release. **TODO** (matches the P1 "pin the gateway compose/image digest"
  item above).
- **Platform/OS provenance** (guest OS, kernel + cmdline, firmware/TEE module →
  reviewed reproducible build): **TODO** — the dstack guest OS image / RTMRs are not
  pinned to a reviewed reproducible build.
- **TCB status / freshness**: **TODO** — the dstack-verifier currently accepts
  `OutOfDate`; set and enforce a policy per criterion 14.

### Open Questions

- Can we run NEAR's `nearai-cloud-verifier` end to end against the live gateway
  report and recover the exact source commit/image digest?
- Which public NEAR models are guaranteed to be `inference_url` TEE-backed
  models, and can this be listed or proven without trial requests?
- Does ordinary chat-completions without conversation state write request or
  response bodies to the JSONB persistence path?
- Can NEAR expose a gateway-owned trusted model catalog claim so Private AI Gateway can
  avoid probing every model individually?
