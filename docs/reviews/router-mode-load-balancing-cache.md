# Router-Mode Review: Load Balancing, Routing, and Cache Locality

Scope: load balancing, routing behavior, retries, overload handling, and
KV/prompt-cache locality for the two router-mode providers we accept today -
Tinfoil `inference.tinfoil.sh` and NEAR AI `cloud-api.near.ai`. Soundness and
end-to-end privacy are covered in a sibling note; this one assumes the router
itself is a trusted workload and asks only how it places traffic across
downstream model CVMs.

Reviewer: private-ai-gateway working tree, 2026-05-18 UTC.

## Summary Conclusion

| Provider | Basic load balancing | Cache-aware routing | Observability of backend | Verdict |
| --- | --- | --- | --- | --- |
| Tinfoil router | Random with overload + circuit-breaker skip | None | `Tinfoil-Enclave` header on every response | Acceptable for stateless OpenAI-compatible traffic. Not acceptable to claim prompt-cache locality. |
| NEAR cloud-api gateway | Per-prefix bucket -> pinned H2 connection -> L4 passthrough | Per-message-prefix trie, 64 default buckets | None (no backend header, no cache-hit counter, no bucket ID in response) | Acceptable with conditions. Cache locality exists in code but is unobservable from outside, and the exact upstream load-balancer mapping is not documented. |

Both providers are acceptable for the request paths Private AI Gateway currently uses
(non-sticky chat completions, sticky `/v1/signature/{chat_id}` lookups). Our
gateway must not advertise cache-aware routing on top of either provider
without operator-visible evidence per request.

## Repos and Commits Inspected

| Provider | Repo | Commit | Path |
| --- | --- | --- | --- |
| Tinfoil router | `tinfoilsh/confidential-model-router` | `41b2e93` (image tag `v0.0.102`) | `/tmp/confidential-model-router` |
| NEAR gateway | `nearai/cloud-api` | `057135f` | `/tmp/nearai-cloud-api` |
| NEAR in-CVM router | `nearai/vllm-router` | `cfd353e` | `/tmp/nearai-vllm-router` |
| NEAR base image | `nearai/private-ml-sdk` | `25c2502` | `/tmp/private-ml-sdk` |
| NEAR verifier | `nearai/nearai-cloud-verifier` | `8b7830e` | `/tmp/nearai-cloud-verifier` (referenced; not inspected for this note) |

Public source for the NEAR AI gateway is `nearai/cloud-api`; this is the actual
binary served behind `cloud-api.near.ai`. Confirmed by `/v1/attestation/report`
returning `gateway_attestation.vpc.vpc_hostname = "cloud-api-prod-mhxybla8.dstack.internal"`
and `info.app_id` consistent with a dstack-launched build of that crate.

There is no separately maintained gateway source. The earlier hypothesis in
`docs/router-mode-provider-review.md` that "we still need to confirm the exact
source and provenance story for the gateway" is partially closed: source is
`nearai/cloud-api`; release-tag-to-attestation provenance is *not* yet pinned
in our adapter and is out of scope here (covered in the soundness note).

## Tinfoil Router

### Routing Algorithm

`/tmp/confidential-model-router/manager/manager.go:311-343` - `Model.NextEnclave(skip)`:

- Partition configured enclaves into three sets: `preferred` (closed circuit,
  not in skip, not overloaded), `closed` (closed circuit, not in skip), `all`.
- Open-circuit enclaves whose cooldown elapsed get *one* probe and are
  returned directly (`circuitbreaker.go:71-80`).
- Otherwise pick uniformly at random from the strongest non-empty tier.

This is round-robin-by-uniform-random, not weighted, not session-aware, not
prefix-aware. There is no read-side state about which enclave a previous
request from the same client went to.

The selection loop in `main.go:574-583` calls `NextEnclave` up to
`EnclaveCount()` times, adding overloaded picks to `skip` between iterations,
then either forwards to the chosen enclave or returns HTTP 429 with a
`Retry-After` derived from per-model `RetryAfterMinutes` config.

### Health, Overload, Circuit Breaker

`manager/metrics.go`:

- Per-enclave goroutine polls `https://<enclave>/metrics` every 15 s.
- Parses `vllm:num_requests_waiting` (`extractWaiting`, `metrics.go:170-188`).
- Compares to `OverloadConfig.MaxRequestsWaiting`; sets the atomic
  `overloaded` flag and increments Prometheus counters
  (`OverloadEventsTotal`, `RecoveryEventsTotal`).
- `ShouldReject` returns `(true, retryAfter, waiting)` when the sample is
  fresh (< 45 s) and at-or-above threshold; otherwise allows the request.
- Stale samples (collected > 45 s ago) are treated as "allow" - fail-open on
  the metrics scrape itself.

`manager/circuitbreaker.go`:

- 3 consecutive failures opens the breaker (`cbFailureThreshold = 3`).
- 30 s cooldown (`cbCooldown = 30 * time.Second`), then `NeedProbe()`
  CAS-transitions Open -> HalfOpen and lets exactly one request through.
- Success while HalfOpen closes the breaker via `RecordSuccess()`
  (`manager/proxy.go:149-153`).

Failure classification in `manager/proxy.go:51-78` (`classifyProxyError`) maps
context-cancel ("client cancellation") *not* to a failure - it goes to
`ClientCancellationsTotal` and skips `cb.RecordFailure()`. This is correct and
prevents client-side aborts from tripping the breaker.

### Retry and Partial-Send Behavior

The router does *not* retry once a request has been sent to a backend.

`main.go:574-587` only iterates `NextEnclave` until a non-overloaded enclave
is picked. The actual forward is a single `enclave.ServeHTTP(w, r)` call
backed by `httputil.NewSingleHostReverseProxy` (`manager.go:358-381`,
`manager/proxy.go:172-200`).

- Transport-level error before any bytes flowed -> `ErrorHandler` writes 502
  to the client (`manager/proxy.go:183-200`). No re-pick; the client retries.
- Backend stalls > 60 s on response headers -> `slowHeadersTotal` is
  incremented passively; the request continues; no kill, no re-pick
  (`manager/proxy.go:130-135`, `manager/proxy.go:155-171`).
- Mid-stream cancellation -> emits `canceled` failure label, does *not* trip
  breaker.

This is conservative and correct: a single LLM completion is not idempotent
on partial output, so blind re-pick during streaming would risk duplicate or
torn responses. The trade-off is that the client must own retry logic.

### Cache Locality

There is no prompt-cache or KV-cache awareness in the router. Same prompt
hashed at the client maps to a uniformly random enclave each time.

For `gpt-oss-120b` (3 enclaves in `config.yml`), live probes confirm this:
8 consecutive identical requests in our probe distributed across all three
enclaves roughly evenly (gpt-oss-120b-0: 2, gpt-oss-120b-1: 3, gpt-oss-120b-2:
3 in one trial). Even an unauthenticated 401 carries the picked
`Tinfoil-Enclave` header, so an attacker cannot tell from the wire whether
the router would actually have routed differently for valid auth - the random
selection happens before auth.

`kimi-k2-6` has a single enclave in `config.yml`, so all repeat probes hit
`kimi-k2-6-inf10.tinfoil.containers.tinfoil.dev` and the model's local prompt
cache reports 16/24 prompt tokens cached on the second request onward. That
is single-instance vLLM prefix caching, *not* router cache-aware routing.

`promptTokenDetailsMetricsModel = "kimi-k2-6"` (`manager/proxy.go:45`) is the
*only* model for which `cached_prompt_tokens` is surfaced in the
`X-Tinfoil-Usage-Metrics` trailer. For every other model, cache-hit metadata
is intentionally hidden, so external observers cannot measure prompt-cache
locality even when it happens to exist.

### Backend Observability

`Tinfoil-Enclave: <host>` response header is set unconditionally in
`manager/manager.go:359`, including on error paths. The full set of
configured enclaves for a model is in the public `config.yml`. This is enough
for us to verify *which* enclave served a given request, which is the
minimum bar for a router-mode trust receipt.

### Risks and Open Questions

- Router restart loses circuit-breaker state and overload samples. Recovery
  is immediate but a brief flapping window is possible.
- `ShouldReject` fail-opens on stale samples (> 45 s old). A backend that
  silently stops exporting `/metrics` will keep receiving traffic.
- Single in-flight `ServeHTTP` per request: when the chosen enclave's TLS
  handshake or H2 connect succeeds but request-body upload then stalls, the
  router cannot reroute without breaking the client connection.
- `recordSuccess()` runs in `ModifyResponse` on `< 500` status only
  (`proxy.go:204-208`). HTTP 4xx from the backend resets consecutive
  failures to zero. A misconfigured backend that returns 4xx for every
  request will *not* trip the breaker.

## NEAR AI Cloud-API Gateway

### Architecture in Brief

There are two routing layers between a client and a NEAR model CVM:

1. **Outer**: `cloud-api.near.ai` (`nearai/cloud-api`, Rust). Runs inside a
   dstack TEE. Implements the **PrefixRouter** described below and pins each
   bucket to a verified vllm-proxy backend via L4 passthrough.
2. **Inner**: `nearai/vllm-router` running inside the model CVM, picking
   among multiple vLLM engine processes on the same host using
   `--routing-logic prefixaware` (per `docker-compose.yaml`).

For the user-visible "router mode" question, only the outer router matters:
that is the one whose attestation we verify and whose load-balancing
behavior is observable on the wire. The inner router is part of the model
CVM workload and is not separately verified by Private AI Gateway today.

### Routing Algorithm - PrefixRouter

`/tmp/nearai-cloud-api/crates/inference_providers/src/vllm/prefix_router.rs`:

- Per-provider trie keyed on message hashes. Each child of the root gets a
  *fresh* bucket id (`next_bucket.fetch_add(1) % num_buckets`); deeper trie
  nodes inherit the parent bundle's bucket
  (`prefix_router.rs:115-141`).
- Default `num_buckets = 64`, `max_depth = 8` messages, both env-tunable.
- Hashing folds in `role` and message `content` (string or array of text
  parts) into a `DefaultHasher` per message (`prefix_router.rs:144-174`).
  Same first-message text -> same bucket; different system prompts -> with
  high probability different buckets (modulo the `% num_buckets` collision
  ceiling).

This is genuine cache-aware routing for the common case "long system prompt,
short user message" - exactly the shape where vLLM prefix caching pays
off. It is *not* KV-cache-aware in the deeper sense of querying live KV
state on the backend; the router's only signal is the prompt itself.

### Bucket -> Backend Binding

`vllm/mod.rs:271-298`:

- `bucket_clients: Vec<Option<reqwest::Client>>`, one per bucket. Each is a
  reqwest client built with `pool_max_idle_per_host(1)` and bucket
  keepalive applied (`bucket_keepalive::apply`, `vllm/mod.rs:285-294`).
- Backend identity comes from a single `base_url`. The actual vllm-proxy
  instance behind that hostname is chosen by an upstream **L4
  passthrough** load balancer at TCP-connect time. Once the bucket client
  successfully connects, its H2 connection (and therefore its backend) is
  held open via keepalive - repeat requests on the same bucket reuse the
  same TCP connection, which on a sane L4 LB means the same backend.

The L4 mapping itself is opaque from outside the dstack network; it is the
trusted bit that turns "same bucket" into "same backend" into "prompt cache
hit." The PrefixRouter alone, without sticky TCP, would not give cache
locality.

### Inline Verification and Bucket Trust State

`vllm/mod.rs:417-545` - `get_or_verify_bucket_client`:

- First use of a bucket: acquire a slot on
  `verification_semaphore` (default size 4, env-tunable
  `INLINE_VERIFY_CONCURRENCY`); call `BackendVerifier::create_verified_client`
  which connects to a backend over TLS, fetches its attestation report,
  verifies it, and pins the SPKI fingerprint into the shared
  `FingerprintState` (`SharedTlsRoots`).
- Up to 3 attempts (`INLINE_VERIFY_RETRIES = 2` plus the initial try).
- If all attempts fail and *some* fingerprint is already pinned, the bucket
  falls back to a shared `fallback_client` that still enforces SPKI pinning
  but is no longer pinned to a specific backend. The explicit code comment is
  `serving with fallback client` (`vllm/mod.rs:529-534`). Prefix-cache
  locality is dropped for that request.
- In bootstrap (no fingerprint pinned yet), fallback is refused and the
  request fails (`vllm/mod.rs:535-544`).

The fallback-client path is the one degradation case to call out: under
sustained attestation flakiness, NEAR will serve requests across whichever
backend the L4 LB hands the fallback client, breaking prefix locality and,
more importantly, breaking the soundness assumption that each request lands
on a specifically *verified* backend. The TLS verifier still enforces "some
already-pinned fingerprint" - so this is not an attestation bypass - but it
is observability the gateway does not surface to clients.

### Retry, Reroute, and Partial-Send Behavior

`vllm/mod.rs:944-955` (streaming) and `vllm/mod.rs:1017-1040` (non-streaming):

- On a non-timeout connection error or `does not match any attested
  fingerprint`, the bucket client is cleared, re-verified, and the request
  is retried *once*.
- A read/request timeout is *not* retried: the comment is explicit that "a
  re-send hits the same model with the same prompt" and that re-trying a
  request that already burned a full timeout would double the cost.
- Connect-timeout *is* retried (treated as transient).
- Both retries happen before the request body has been streamed to the
  backend, so partial-send is not a concern in practice.
- There is no cross-bucket retry. The bucket-to-backend pin is preserved
  across retries; reroute means re-verify-same-pin, not pick-a-different-pin.

### Chat ID Stickiness for Signature Fetch

`vllm/mod.rs:780-798`, `vllm/mod.rs:702-735`:

- For non-streaming, the gateway captures `chat_id` from the response body
  immediately and records `signature_buckets[chat_id] = bucket_id`.
- For streaming, the gateway records `pending_buckets[request_hash] =
  bucket_id` at send time and promotes it to `signature_buckets[chat_id]`
  once the chat id appears in the stream.
- `/v1/signature/{chat_id}` looks up the bucket and re-uses the same
  pinned bucket client to fetch the model's signed transcript. On 404, it
  is allowed to *clear* the binding and fall through to broadcast
  (`vllm/mod.rs:727-735` - not fully inspected here; the
  `unpin_chat_connection` path exists).

This is the only point where NEAR has explicit per-session affinity. It is
about retrieving a model-side signature, not about KV-cache hits.

### Backend Observability - Live Probes

Live probes against `https://cloud-api.near.ai` 2026-05-18 06:17-06:18 UTC,
authenticated with our `NEARAI_API_KEY`:

- `/v1/chat/completions` returns `inference-id: <uuid>` per request and no
  backend identifier. No bucket id. No `cached_prompt_tokens`.
- `/v1/attestation/report` returns gateway-only attestation
  (`gateway_attestation.{intel_quote, signing_address, event_log, info,
  vpc}`). No per-backend report and no list of currently active backends.
- `/v1/signature/{chat_id}` exists; tested with bogus chat id -> 404
  `{"error":"Signature not found: ...:ecdsa"}`.

So from the wire, a Private AI Gateway user cannot tell:

- which backend served a given request;
- whether the prefix-cache routing actually pinned consecutive identical
  prompts to the same backend;
- whether the request was served by a verified bucket or by the fallback
  client.

This is the central observability gap for NEAR router mode.

### Cache Locality - Code vs. Wire

Cache locality exists in code: `PrefixRouter::route` is deterministic over
the message-prefix hash, and the bucket-client's H2 keepalive holds the TCP
connection open across requests on the same bucket. Under steady operation,
the same system prompt repeatedly hitting the same gateway instance will
land on the same backend.

We cannot independently verify this on the wire today. The only signal we
could derive is timing - if the backend has a fast warm path and a slow
cold path, repeated identical prompts under one gateway instance should
stabilize at the warm latency. That is a noisy signal and not a substitute
for a backend identifier or a cache-hit counter.

Two failure modes break locality silently:

1. The fallback client takes over on attestation flakiness - same bucket,
   *different* (unpinned) backend.
2. The gateway has multiple replicas behind `cloud-api.near.ai`; each
   replica has its own `prefix_router` and `bucket_clients`. The same
   prompt hitting replica A vs. replica B routes via independent trie
   instances. The `/v1/attestation/report` gateway hostname
   (`cloud-api-prod-mhxybla8.dstack.internal`) is one specific instance,
   but the public hostname is load-balanced. We have no evidence that the
   client gets pinned to a single gateway replica - TLS session resumption
   plus connection pooling on the user's side help, but a fresh TCP
   connection can land on a different gateway replica.

### Risks and Open Questions

- Fallback-client path silently drops prefix-cache routing on attestation
  flakiness. Gateway cannot detect this from response headers.
- L4 passthrough mapping (bucket TCP -> vllm-proxy instance) is not part of
  any public document we found. The cache-locality claim depends on this
  mapping being stable across requests.
- The public gateway hostname is fronted by what appears to be a
  load-balanced set of dstack instances. Multiple gateway replicas each
  have independent `PrefixRouter` and bucket maps, so cache-locality is
  per-replica, not global.
- `chat_id`-based stickiness for `/v1/signature` does not extend to
  `previous_response_id` or any "continue this conversation" affinity for
  the next chat completion. A user resuming a session can land on a
  different backend than the one that served the first turn unless the
  prompt prefix still maps to the same bucket.
- `num_buckets = 64`. Two different system prompts can collide on the same
  bucket (birthday paradox) and unnecessarily share a backend; not a
  correctness problem, but it lowers cache efficiency and bunches load.

## Live and Unit Tests Performed

All commands run from the `private-ai-gateway` repo root
at 2026-05-18 06:16-06:18 UTC. Output captured inline above; no separate
artifact files written for this note (cheap, repeatable curl probes).

| Test | Command (abbreviated) | What it checked |
| --- | --- | --- |
| Tinfoil 401 backend disclosure | `curl -s -i POST inference.tinfoil.sh/v1/chat/completions ... Bearer test ...` | `Tinfoil-Enclave` header set even on auth failure -> backend pick happens before auth. |
| Tinfoil multi-instance random spread | 6 unauthenticated POSTs against `gpt-oss-120b` | Spread across `gpt-oss-120b-{0,1,2}` confirms uniform-random pick. |
| Tinfoil authed multi-instance | 8 authed POSTs, same system prompt, varying user msg | Same spread across all 3 enclaves - system-prompt does *not* pin a backend. |
| Tinfoil prompt-cache visibility | `kimi-k2-6` repeats with `X-Tinfoil-Request-Usage-Metrics: true` | `cached_prompt_tokens=16/24` from 2nd request - but only because `kimi-k2-6` is single-enclave. Exposed only for this one model. |
| NEAR auth gate | Unauthenticated `/v1/models` | 401 - every endpoint is auth-gated. |
| NEAR authed completion | One authed POST, `google/gemma-4-31B-it` | `inference-id` header, no backend identifier, no cache counter, status 200. |
| NEAR attestation surface | `GET /v1/attestation/report` authed | Returns single `gateway_attestation` with TDX quote, `app_id`, `vpc_hostname`. No per-backend list. |
| NEAR signature shape | `GET /v1/signature/test-not-real-id` authed | 404 `{"error":"Signature not found: test-not-real-id:ecdsa"}` - confirms chat-id keyed lookup. |

Test designs we did *not* run but recommend:

- **Tinfoil locality null test**: 100 sequential identical-prompt requests
  against a 3-enclave model; assert chi-square of `Tinfoil-Enclave`
  distribution against uniform. Confirms (or refutes) that random pick is
  actually uniform.
- **Tinfoil overload skip**: configure a synthetic high `MaxRequestsWaiting`
  load against one enclave and assert other enclaves carry traffic
  according to `NextEnclave` skip semantics. Requires staging access; not
  feasible against production.
- **NEAR prefix-cache locality**: send N requests with identical 8k-token
  system prompt and unique short suffix; measure TTFB. If locality holds,
  TTFB should drop from the cold value to the warm value after the first
  request and stay there. With NEAR's current observability surface this
  is the *only* available signal.
- **NEAR replica scatter**: from many different client IPs (or with
  `Connection: close`), repeat the locality test. If gateway-replica
  pinning is not in play, the warm-TTFB stability should degrade.

The locality tests above need a budget (8k-prompt requests cost real
tokens) so they should be run once during adapter sign-off and re-run only
when NEAR changes the gateway image digest.

## Recommended Changes to Gateway/Provider Adapter

These are observations for the upstream maintainers, not implementation
asks for this review. The reviewer who owns the Tinfoil/NEAR adapter
should weigh them.

1. **Gateway must not advertise prompt-cache awareness in router mode.**
   The current `RoutingUpstreamVerifier` only attests the gateway. Surface
   load-balancing as "best-effort" in receipts; do not infer cache
   locality from provider verification.

2. **Tinfoil adapter: record `Tinfoil-Enclave` per request.** Today the
   gateway does not extract this header (no hits for `tinfoil-enclave`
   in `src/`). Recording it in the receipt is cheap, makes router-pick
   auditable, and lets users replay a session against the same enclave
   for signature verification.

3. **NEAR adapter: ask NEAR to surface bucket id or backend identifier in
   response headers.** Without it, the cache-locality claim is
   code-review-only. A header such as `X-NearAI-Bucket: <0..63>` plus
   `X-NearAI-Backend: <opaque-stable-id>` would let us confirm same-prompt
   stickiness on the wire and detect fallback-client degradation.

4. **NEAR adapter: pin the gateway image digest in our config.** The
   gateway's `app_id` from `/v1/attestation/report` is recoverable per
   request; we should record it in the verification lease and refuse
   forwarding if it changes mid-session. This is in scope for the
   soundness review, not load-balancing - noted here only so it does not
   get lost between reviews.

5. **Live-test harness: add the two recommended probes above to
   `scripts/live_e2e/` as opt-in nightly checks**, gated by a flag because
   they spend non-trivial token budget. Tag the artifacts with the
   gateway `app_id` and Tinfoil image tag so we can spot a provider
   redeploy that quietly changes behavior.

## Confidence Notes

- "Tinfoil router uses random pick with circuit-breaker / overload skip"
  - high confidence, code + live probes agree.
- "Tinfoil router has no prompt-cache awareness" - high confidence; no
  trie, no session key, no prefix hashing anywhere in `manager/`.
- "NEAR gateway implements PrefixRouter with sticky H2 buckets via L4
  passthrough" - high confidence from the source. Low confidence that
  this code is the binary actually serving production traffic *for every
  model on every replica* (`app_id` matches but we did not enumerate
  replicas or compare image digests).
- "NEAR cache locality is not externally observable" - high confidence
  from the live probes.
- "Tinfoil does not retry once forwarding starts" - high confidence; the
  single `ServeHTTP` call and the `ErrorHandler` semantics are clear.
- "NEAR retries exactly once on connection/fingerprint error and never on
  request timeout" - high confidence from the explicit comments and the
  send-closure structure.
