# Upstream Verification Lifecycle

This page explains when the gateway verifies an upstream, how long a verified result is reused, and how channel-binding failures affect forwarding. It applies to the direct-upstream configuration managed by `UpstreamConfigManager`.

For field definitions and defaults, see [Configuration reference](configuration-reference.md). For the resulting audit record, see [Attested sessions](attested-session-system.md).

## Security property

A request is fail closed only when it requires ACI verification:

- `provider.aci_verified` is `true`;
- `provider.aci_session_ids` is a non-empty allowlist; or
- middleware marks the selected route as TEE-only.

For a required request, the gateway does not forward the prompt unless a verifier returns `verified` and the current request can be sent through an enforced channel binding. An unconstrained request can still run the configured verifier and record its result, but a failed or missing result does not by itself block forwarding.

## Verification keys and scope

The verifier input contains the upstream config name, origin, upstream model identifier, hash of the body that will be forwarded, and whether verification is required.

Caching follows the provider's attestation scope:

- Router-scoped providers use one verification key for the origin. The model is omitted because all routed models share the same attested channel.
- Per-model or per-instance providers include the model in the verification key.

Only successful verification events are cached. Failed verification is returned to the caller and is not stored as a reusable success.

## Startup and configuration replacement

After startup, the manager prewarms verification for configured targets. Replacing upstream configuration through the admin API constructs and validates a new runtime snapshot, publishes it, and starts another prewarm.

For a router-scoped upstream, prewarm chooses one deterministic representative model. For per-model providers, it verifies each distinct upstream model.

A successful prewarm also records the corresponding attested session. This makes the audit surface useful before the first user request. Request-time verification records the same content-addressed session idempotently.

Prewarm results are logged after the background task finishes. They are not
included in the startup or admin response. A failed prewarm does not prevent
the process from serving unrelated routes. A later constrained request still
applies the fail-closed gate.

## Cache and background refresh

`verifier_cache_seconds` controls the maximum reuse period for provider-verifier results. Its default is 300 seconds.

`verification_refresh_seconds` controls proactive refresh:

- omitted: refresh at `max(verifier_cache_seconds - 60, 1)` seconds;
- positive integer: use that interval;
- `0`: disable proactive refresh for that upstream.

The manager runs at the smallest enabled interval and refreshes only upstreams whose policy enables refresh. Refresh bypasses the existing cache. A successful result replaces the cached event; a failed refresh leaves the previous unexpired successful event in place.

The ACI-service verifier also limits its cached result to the attestation report's `stale_after` timestamp. Its usable lifetime is the earlier of that timestamp and the configured cache deadline.

Cache lifetime is not a promise that a connection remains safe for that duration. Every forward still enforces the cached channel binding against the connection it uses.

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

A channel-binding mismatch can indicate normal rotation or an attack. The gateway handles it as a state transition that must be reverified:

1. Invalidate the cached verifier event owned by the gateway.
2. Run one fresh verification.
3. Retry only if the new result verifies and its binding can be enforced.
4. Treat another mismatch as terminal for that candidate and leave the stale cache entry invalidated.

Caller-supplied verification events are not placed in the gateway cache, so the gateway does not invalidate or silently replace them.

For a request with an explicit session allowlist, the allowlist is applied again to the freshly derived sessions. A rotated binding therefore cannot pass by citing its historical session identifier.

## Chutes provider sessions

Chutes has a second lifecycle because its router discovers a changing set of attested instances. `session_refresh_seconds` controls proactive provider-session refresh:

- omitted for Chutes: 45 seconds;
- positive integer: use that interval;
- `0`: disable proactive session refresh.

The refresh job obtains the verified provider event, refreshes model session nonces through the Chutes backend, and records a result per model. If the backend finds a channel-binding mismatch, the manager forces verifier refresh before considering the session refreshed.

This provider-session refresh is separate from the general verifier cache refresh. One maintains instance discovery and nonces; the other renews the attestation result used to authorize those instances.

## Failover interaction

Middleware mode can evaluate several candidate routes. Verification failure on a route required to be attested makes that candidate ineligible. The router can try another eligible candidate without forwarding the prompt to the failed one.

After a request reaches an upstream, the gateway can fail over on configured transient and account-specific statuses: `401`, `402`, `403`, `429`, `500`, `502`, `503`, and `504`. Recognized capacity bodies are also failover signals. Non-basic user tiers can receive one delayed capacity retry within the initial ten-second window.

Each candidate goes through its own verification and binding checks. A verified event for one upstream never authorizes another.

## Operational signals

Use these surfaces when diagnosing lifecycle behavior:

- gateway logs for prewarm, refresh, invalidation, and binding-mismatch messages;
- `GET /v1/aci/sessions` for current materialized sessions;
- `GET /v1/admin/upstreams` for redacted active configuration and its digest;
- `GET /v1/metrics` for gateway-owned request metrics;
- the receipt's `upstream.verified` events for the decision made on a specific request.

Do not infer a successful current verification from the mere presence of an unexpired session. Sessions are audit artifacts. The request path obtains and enforces current verifier state independently.
