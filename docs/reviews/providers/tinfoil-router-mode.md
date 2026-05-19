# Tinfoil Router-Mode Review

Date: 2026-05-18 UTC.
Provider endpoint: `https://inference.tinfoil.sh`.
Router source: `tinfoilsh/confidential-model-router`.
Reviewed commit: `41b2e93e099baf3dd8085066c205f030b280cadc`.
Observed router image: `ghcr.io/tinfoilsh/confidential-model-router@sha256:893f9a33e3c24bced341413a2a934e9a4f0453a76787616d42631b3181c472ba`
(`v0.0.102`).

Source reports:

- [router-mode-soundness.md](../router-mode-soundness.md)
- [router-mode-load-balancing-cache.md](../router-mode-load-balancing-cache.md)

## Verdict

Tinfoil router mode is acceptable with conditions.

The accepted trust model is:

1. The client verifies and pins the confidential router's attested TLS SPKI.
2. The attested router verifies each downstream model enclave before adding it
   to the serving pool.
3. The router forwards to the selected model enclave through TLS pinned to the
   model enclave's attested public key.

The main condition is that Private AI Gateway should pin the audited router
compose/image digest instead of accepting any future Tinfoil router
measurement implicitly. Router verification proves the HTTPS endpoint is a
TEE router; it does not by itself prove the router is the exact audited
source revision unless we bind and allowlist the source/image evidence.

## Criteria Status

Passed:

- The reviewed router verifies model enclave attestation before adding a model
  backend.
- Router-to-model forwarding uses TLS bound to the model enclave's attested
  public key.
- Tool/MCP routing follows the same attested enclave path.
- The normal production build did not include prompt/completion body logging in
  the reviewed paths.
- The router returns selected-enclave information in `Tinfoil-Enclave`, which
  can be recorded for audit.

P0 adapter requirements:

- Verify and pin the router TLS SPKI before forwarding.
- Verify Tinfoil's provider-published Sigstore measurements. The current
  Python verifier already does this for provider-current acceptance.
- Pin the audited router compose/image digest for strict-release runs, instead
  of implicitly accepting whatever Tinfoil publishes as latest.
- Do not set `X-Forwarded-Host` when forwarding through the router.
- Treat `/v1/models` as advisory control-plane metadata, not proof.

P0 TODOs before strict-release inclusion:

- Maintain an allowlist tying router compose/image digest to reviewed source or
  release artifacts.
- Decide whether `UPDATE_CONFIG_URL` without a pinned hash is acceptable
  provider-owned policy or must be rejected in strict mode.
- Confirm production runtime flags cannot enable debug behavior without
  changing the pinned compose identity.

P1 TODOs:

- Record `Tinfoil-Enclave` in receipt provider claims or transparency events.
- Add live tests that verify the selected enclave belongs to the advertised
  model enclave set.
- Keep cache locality marked unclaimed unless Tinfoil adds observable
  cache-aware routing.

## Soundness Findings

Positive evidence:

- The router source is public and the reviewed deployment config pins a
  container image digest in `tinfoil-config.yml`.
- The router verifies model source measurements through Tinfoil's Sigstore
  chain before accepting a model enclave.
- `addEnclave` fetches `/.well-known/tinfoil-attestation`, verifies the
  attestation, verifies hardware measurements for TDX-Guest-V2, and rejects a
  model enclave whose measured source does not match the model's expected
  source measurement.
- Router-to-model forwarding uses
  `tinfoilClient.TLSBoundRoundTripper{ExpectedPublicKey: tlsKeyFP}`.
- Tool/MCP backends use the same attested model-enclave path.
- The production build excludes the developer-only on-disk session logger
  because `local_debug_disabled.go` is the default build path.
- Billing events carry token counts, model, route, request id, enclave, and
  API key metadata, but not prompt or response bodies.
- A grep of the reviewed source did not find prompt/completion body logging in
  the normal router, billing, or toolruntime paths.

Important caveats:

- Runtime config refresh uses `UPDATE_CONFIG_URL` without a required hash by
  default. A malicious or mistaken config change should not let an unverified
  enclave join the pool, but it can affect model-to-enclave steering and
  availability.
- `--debug` is a runtime flag. Enabling it would change compose arguments and
  therefore the measured compose hash, so a compose-hash allowlist should
  catch it.
- `X-Forwarded-Host` participates in subdomain model routing. Our aggregator
  should not set it when forwarding to the router.
- `/v1/models` uses the Tinfoil control plane and is not itself a TEE-bound
  user-content path.

## Load Balancing and Cache Findings

Tinfoil's router performs basic load balancing, not cache-aware routing.

Observed algorithm:

- `Model.NextEnclave(skip)` chooses uniformly at random among healthy,
  non-overloaded enclaves.
- It skips open circuit breakers, probes one half-open backend after cooldown,
  and prefers non-overloaded backends.
- The request path tries up to `EnclaveCount()` picks before returning 429.
- Once `enclave.ServeHTTP(w, r)` is called, the router does not retry against
  another backend. This is the right behavior for non-idempotent streaming
  completions.

Overload and health:

- Per-enclave metrics polling checks `vllm:num_requests_waiting`.
- Fresh queue-depth samples above threshold mark the backend overloaded.
- Stale metrics samples fail open and continue serving traffic.
- Three consecutive transport/backend failures open the circuit breaker; a
  single probe is allowed after 30 seconds.

Cache locality:

- There is no prefix trie, session key, prompt hash, or cache key in the
  router placement path.
- Multi-enclave models should be expected to spread identical prompts across
  instances.
- `kimi-k2-6` showed prompt-cache metadata only because it is currently a
  single-enclave model in the tested config.
- Private AI Gateway should not claim prompt-cache or KV-cache locality for Tinfoil
  router mode.

Backend observability:

- The router returns `Tinfoil-Enclave: <host>` on responses, including error
  paths. This is enough for the aggregator to record which backend was chosen.

## Live Evidence

Live probes during review:

- `GET https://inference.tinfoil.sh/.well-known/tinfoil-proxy` returned the
  model/enclave map with measurements, `tls_key_fp`, and `hpke_key`.
- Multi-instance probes against `gpt-oss-120b` distributed across all three
  configured enclaves.
- Repeated `kimi-k2-6` requests exposed cached prompt token metadata only on
  the single configured enclave.
- Existing aggregator live artifact:
  `/tmp/private-ai-gateway-live-e2e/20260518-053809`.

## Required Adapter Changes

P0:

- Surface and pin the router compose/image digest in the Tinfoil verifier
  result. The allowlist should be maintained by reviewing the corresponding
  router source/release.

P1:

- Record `Tinfoil-Enclave` in the ACI receipt event log or a provider-specific
  receipt extension. This makes the selected downstream enclave auditable.
- Decide whether to strip the raw `Tinfoil-Enclave` response header before
  returning to Private AI Gateway users, while still recording the selected enclave
  internally.

P2:

- Add an opt-in live test that sends repeated identical prompts to a
  multi-enclave Tinfoil model and verifies the observed router selection is
  not cache-affine.
- Add a live test that verifies `Tinfoil-Enclave` belongs to the model's
  advertised enclave set and that the enclave attestation matches the model
  source measurement.

## Open Questions

- Should we treat runtime config updates as acceptable provider-owned policy,
  or require a pinned update URL/hash for router-mode acceptance?
- Should the verifier bridge reject any Tinfoil router compose hash outside a
  known-good allowlist immediately, or only surface it for receipt/audit first?
- Should Private AI Gateway retain selected-enclave information only in receipts, or also
  expose it in a response header for debugging?
