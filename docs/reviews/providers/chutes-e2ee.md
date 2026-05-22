# Chutes E2EE Review

Date: 2026-05-18 UTC.
Provider endpoint: `https://api.chutes.ai`.
Trust boundary: verified Chutes model instance reached through Chutes E2EE
transport.

Source reports:

- [upstream-verification-lifecycle.md](../../upstream-verification-lifecycle.md)
- Chutes catalog and E2EE throughput probes captured during the provider audit.

## Verdict

Chutes is accepted for limited traffic on the E2EE path.

The security model is direct instance binding:

1. The Chutes verifier verifies TDX evidence for a model instance.
2. The quote report data binds freshness and the instance E2EE public key.
3. Private AI Gateway records the E2EE public-key digest as the channel binding.
4. The Chutes backend encrypts the request to a nonce-bearing instance whose
   key digest is covered by the current verification lease.

This is sound for confidentiality and request-path binding when the adapter
uses the E2EE transport. It is not yet ready for general production throughput
because Chutes' public nonce discovery endpoint exposes only a small,
rate-limited subset of instances per call.

## Criteria Status

Passed:

- E2EE public-key material is bound to the attestation evidence.
- Private AI Gateway enforces the E2EE public-key digest before invoking Chutes.
- Nonces are treated as single-use local session material.
- Cold evidence verification is kept off the normal request path by prewarming
  and background refresh.
- Streaming responses are decrypted before normal ACI receipt hashing.
- Chutes catalog lookup now requires an exact model-name match, or a UUID-like
  `chute_id` supplied directly. It no longer falls back to the first returned
  catalog item.
- Production routes can pin configured upstream model ids to concrete
  `chute_id` UUIDs with `chutes_chute_ids`; both the verifier bridge and E2EE
  sender use the same pin before any catalog lookup.

P0 TODOs:

- Resolve the nonce-throughput ceiling before routing meaningful user traffic.
  Current discovery returns at most five instances and ten nonces per instance
  per call, with aggressive 429 behavior.
- Do not claim exact served-weight identity beyond the verified Chutes claim
  unless Chutes exposes a signed or otherwise verified `served_model_id`.

P1 TODOs:

- Add long-window throughput tests across many nonce TTL cycles.
- Expose pool metrics: verified key count, pooled nonce count, refresh
  success/failure, and binding mismatch count.
- Record the Chutes instance id and key id in provider-specific receipt claims
  when available.

## Workload Identity And Binding

Chutes does not expose a separate durable ACI-style identity key for each model
fleet. The practical identity for Private AI Gateway is the verified instance E2EE key.

The verifier expects the TDX report-data field to bind the verifier nonce and
E2EE public key:

```text
report_data[0:32] = sha256(nonce || e2e_public_key)
```

The derived resource is the encrypted invocation. A request is acceptable only
when it is encrypted for an instance key whose digest is present in the current
verified lease.

## Catalog And Model Identity

The public Chutes catalog is not a security root.

Findings:

- Most TEE-flagged catalog entries are inactive miner workloads.
- Production traffic is concentrated in a small set of chutes operated by the
  canonical `chutes` user.
- `name` is not globally unique across the full catalog.
- The current resolver's "first item with any `chute_id`" fallback is unsafe.
- Some display names alias different served weights or quantizations. The
  public API did not expose a signed machine-readable served-model id in the
  snapshot.

Adapter rule:

```text
Exact configured model -> exact chute id -> verified E2EE instance key.
No exact match, no lease.
```

## Session And Throughput

Chutes E2EE uses two freshness layers:

- evidence freshness for verifying instance keys
- invocation nonces for sending encrypted requests

Invocation nonces behave like short-lived tickets. They do not replace
attestation. They are usable only when their instance key is already verified.

Observed discovery behavior:

- `/e2e/instances/{chute_id}` returned at most five instances per call.
- Each instance returned ten nonces.
- `nonce_expires_in` was 60 seconds.
- Polling too aggressively caused 429s on busy chutes.

This makes the high-throughput path provider-limited. The current Private AI Gateway
implementation can prewarm and refresh a nonce pool, but it cannot turn a
five-instance discovery window into full-fleet load balancing.

## Privacy Boundary

The accepted Chutes path is application-level E2EE. Private AI Gateway forwards encrypted
request material to Chutes and decrypts the response in the gateway before
normal ACI receipt hashing.

Plain HTTPS to Chutes is not accepted for ACI-secured traffic unless Chutes
provides a separate attested TLS binding or equivalent router proof.

## Required Adapter Behavior

The Chutes adapter must:

- run provider-owned verification during prewarm or refresh
- record only E2EE key digests that passed verification
- pop only non-expired, locally unused invocation nonces
- match every nonce to a verified key digest before invocation
- fail closed when no verified nonce is available
- keep credential material in the upstream config, not process environment
- record E2EE binding and compact provider claims in receipts

## Open Questions

- Is the five-instance discovery cap intentional, and is there pagination or a
  bulk high-throughput path?
- Can Chutes return signed served-model identity or structured weight metadata?
- Can Chutes expose stable per-instance evidence TTLs and key-rotation rules?
- Should Private AI Gateway support a Chutes managed-router path if Chutes later provides
  attested router TLS binding and full gateway soundness?
