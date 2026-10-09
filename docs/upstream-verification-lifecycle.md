# Upstream Verification Lifecycle

This page explains when the gateway verifies an upstream, how long a verified result is reused, and how channel-binding failures affect forwarding. It applies to the direct-upstream configuration managed by `UpstreamConfigManager`.

For field definitions and defaults, see [Configuration reference](configuration-reference.md). For the resulting audit record, see [Attested sessions](attested-session-system.md).

## Security property

A request fails closed only when it requires ACI verification, as defined in
[Require ACI verification](api-reference.md#require-aci-verification) and
[TEE-only hostnames](configuration-reference.md#tee-only-hostnames).

For a required request, the gateway does not forward the prompt unless a verifier returns `verified` and the current request can be sent through an enforced channel binding. An unconstrained request can still run the configured verifier and record its result, but a failed or missing result does not by itself block forwarding.

## Verification keys and scope

The verifier input contains the upstream config name, origin, upstream model identifier, hash of the body that will be forwarded, and whether verification is required.

Caching follows the provider's attestation scope:

- Router-scoped providers use one verification key for the origin. The model is omitted because all routed models share the same attested channel.
- Per-model or per-instance providers include the model in the verification key.

Only successful verification events are cached. Failed verification is returned to the caller and is not stored as a reusable success.

## Cache and background refresh

The manager runs one sequential task per upstream with a verifier route and
refresh enabled. Startup verification uses this task; hot reload cancels old
tasks and starts fresh verifiers. Startup and admin responses do not wait for it.
Router-scoped and ACI-service upstreams refresh one representative model;
other providers refresh each distinct upstream model.

Never-attempted targets run first in stable order, then due warm entries by
earliest expiry, then due cold retries by oldest attempt. Targets are re-selected
after waiting for a background permit, since caches and deadlines can change.
The period p is a positive `verification_refresh_seconds`, or
`max(verifier_cache_seconds - verifier_request_timeout_seconds, 1)` by default
(240 seconds); zero disables refresh. The lead is `max(TTL - p, 0)`,
and warm entries become due at expiry minus lead. External and ACI-service cache
TTLs begin at verification start. ACI-service caps expiry at keyset `not_after`,
rechecked when verification completes; appraisal uses the wall clock after the
response. Cold requests and background refresh share its single-flight lock.

All tasks share `upstream_verification_concurrency` permits (default 4);
request-time verification is independent. After success, a target cannot start
again before its previous start plus p, including cacheless verifiers. Failure
keeps a valid entry and retries at expiry, otherwise no earlier than start plus p.
On Unix, cancellation or timeout kills an external verifier's process group,
including bridge subprocesses, and reaps the child.

Warmth is best-effort within capacity: replacements must complete before expiry;
under sustained upstream overload cold targets may get no background retry,
while requests still verify on demand. A previously successful target found cold
at refresh start logs a warning with upstream and model. Every forward enforces
the current channel binding against its connection.

## Request-time flow

For a constrained request, the gateway follows this sequence:

1. Resolve the candidate upstream and the body that would be forwarded.
2. Obtain a cached successful verifier event or perform verification.
3. Require a verified result.
4. Derive current attested sessions and apply any `aci_session_ids` allowlist.
5. Connect through a client that enforces the verified channel binding.
6. Forward the prompt only after the binding is satisfied.
7. Record the verification event and selected session in the signed receipt.

TLS SPKI bindings are enforced by the pinned TLS client. Chutes E2EE public-key bindings are enforced by the provider backend when it selects and encrypts to a verified instance.

## Binding mismatch and reverification

A channel-binding mismatch can indicate normal rotation or an attack. The gateway treats it as a state change that must be verified again. Invalidation works the same way for every provider verifier, including `aci-service`.

1. Invalidate the cached verifier event owned by the gateway.
2. Run a fresh verification that bypasses the cache.
3. Retry the forward only if the new result verifies and its binding can be enforced.
4. Repeat for a later mismatch, up to two fresh verifications per candidate.
5. Treat a mismatch after the second fresh verification as terminal for that candidate, and leave the cache entry invalidated.

Caller-supplied verification events are not placed in the gateway cache, so the gateway does not invalidate or silently replace them.

For a request with an explicit session allowlist, the allowlist is applied again to the freshly derived sessions. A rotated binding therefore cannot pass by citing its historical session identifier.

## Chutes provider sessions

Chutes has a second lifecycle because its router discovers a changing set of attested instances. `session_refresh_seconds` sets the proactive provider-session refresh cadence; see [Upstream fields](configuration-reference.md#upstream-fields) for its default.

The refresh job obtains the verified provider event, refreshes model session nonces through the Chutes backend, and logs one `upstream provider session refresh finished` line per model. If the backend finds a channel-binding mismatch, the manager forces a verifier refresh before it counts the session as refreshed. That result is `refreshed_via_verifier`, and it always reports `refreshed_nonces: 0`, even when the verifier recorded fresh nonces.

This provider-session refresh is separate from the general verifier cache refresh. One maintains instance discovery and nonces; the other renews the attestation result used to authorize those instances.

Provider-session material is subordinate to the verification result. A pooled
nonce cannot extend trust after verification expires, and every selected
instance must still match a current verified E2EE-key binding.

Chutes' `/e2e/instances` returns a sample of instances, and only nonces for
instances in the verified key set are usable. More
`chutes_e2ee_discovery_rounds` widen that overlap but cannot make it
deterministic, so the pool can run short while verification is healthy.

For each request, the Chutes backend:

1. Resolves the provider model to a chute ID, with a five-minute local cache.
2. Removes one unexpired, single-use nonce whose instance ID and E2EE public-key
   digest match the current verified binding set.
3. Refills the pool through verified instance discovery when no matching nonce
   remains.
4. Encrypts the request to the selected instance, as described in
   [Encrypted transport](providers/chutes/verification.md#encrypted-transport).

The pool uses the provider's `nonce_expires_in` value when present and a
55-second fallback otherwise. Expired entries are discarded, and selecting a
nonce removes it from the pool. The model cache and nonce pools are in memory;
a process restart rebuilds them through background refresh or the next request.

A dated live probe on 2026-05-18 observed roughly 138 to 145 seconds for cold
Chutes evidence verification and roughly one second for warmed small prompts.
A short 120 requests-per-minute stage and a 25-request burst completed without
provider `429` responses. These measurements are a lower bound from one model
and account, not a current latency or throughput guarantee. They explain why
background verification and session refresh keep evidence discovery off the normal
request path.

To measure warmed Chutes throughput across several nonce lifetimes, run the
rate probe against a provider matrix that contains one Chutes entry:

```bash
python3 scripts/live_e2e/chutes_rate_probe.py \
  --providers-file /tmp/chutes-provider.json \
  --provider chutes \
  --stage 120@0.5 \
  --stage 180@0.333 \
  --burst-concurrency 20 \
  --warmup 1 \
  --keep-going-after-429 \
  --port 0
```

Each `--stage` is `COUNT@INTERVAL_SECONDS`, and `--port 0` picks a free port.
Besides provider `429` responses, watch whether session refresh keeps the nonce
pool filled after the first minute and whether any request falls back to slow
evidence discovery. For the `--providers-file` layout, see
[Provider matrix format](live-e2e-test-suite.md#provider-matrix-format).

## Failover interaction

Middleware mode can evaluate several candidate routes. Verification failure on a route required to be attested makes that candidate ineligible. The router can try another eligible candidate without forwarding the prompt to the failed one.

After a request reaches an upstream, the gateway fails over on the provider statuses and capacity signals listed in [Middleware failover behavior](api-reference.md#middleware-failover-behavior), and can retry capacity-failed candidates once after a short delay.

Each candidate goes through its own verification and binding checks. A verified event for one upstream never authorizes another.

## Operational signals

Use these surfaces when diagnosing lifecycle behavior:

- gateway logs for refresh, invalidation, and binding-mismatch messages;
- `GET /v1/aci/sessions` for current materialized sessions;
- `GET /v1/admin/upstreams` for redacted active configuration and its digest;
- `GET /v1/metrics` for gateway-owned request metrics;
- the receipt's `upstream.verified` events for the decision made on a specific request.

Do not infer a successful current verification from the mere presence of an unexpired session. Sessions are audit artifacts. The request path obtains and enforces current verifier state independently.
