# Upstream Verification and Lease Lifecycle

This note records the current high-level design and the Chutes throughput
findings from the live probes on 2026-05-18 UTC. It is implementation-facing:
ACI should stay simpler than this document.

## What We Proved

The gateway can run Chutes through config-backed credentials and E2EE
transport without provider credentials in the gateway process environment.
The live launcher writes the provider key into upstream config as
`bearer_token`, starts the gateway, and strips the provider key env var from
the child process. The Rust Chutes adapter passes the config-backed key and
Chutes tuning to the verifier bridge as structured stdin input.

Live artifacts:

- Config-path sanity: `/tmp/private-ai-gateway-live-e2e/20260518-042641-chutes-rate-probe`
- Warmed ramp: `/tmp/private-ai-gateway-live-e2e/20260518-043114-chutes-rate-probe`
- Lease/session refresh check: `/tmp/private-ai-gateway-live-e2e/20260518-052653-chutes-rate-probe`
- Tinfoil TLS-binding lifecycle: `/tmp/private-ai-gateway-live-e2e/20260518-053809`
- NEAR AI TLS-binding lifecycle: `/tmp/private-ai-gateway-live-e2e/20260518-053819`

Measured against `google/gemma-4-31B-turbo-TEE` through the public alias
`gemma-chutes`:

| Stage | Requests | Result | Rate limit | Latency |
| --- | ---: | --- | ---: | --- |
| Cold verification warmup | 1 | 200 | 0 | 138.247s |
| Fixed 60 rpm | 10 | 10/10 200 | 0 | avg 1.052s, max 1.453s |
| Fixed 120 rpm | 20 | 20/20 200 | 0 | avg 1.171s, max 2.057s |
| Burst, concurrency 10 | 25 | 25/25 200 | 0 | avg 1.152s, max 1.649s |

The lease/session refresh check used explicit upstream config fields:
`verification_refresh_seconds: 0`, `session_refresh_seconds: 30`, and
`chutes_e2ee_discovery_rounds: 3`. It sent two warmed requests 65 seconds
apart, beyond the local fallback nonce TTL. Both requests stayed fast:
1.543s and 1.062s. Gateway logs showed background session refreshes at
05:29:13, 05:29:43, and 05:30:13 UTC with 20, 30, and 10 refreshed nonces.

This is a lower bound, not a ceiling. It proves the warmed path can sustain at
least a short 120 rpm stage and a 25-request burst without 429s. It does not
yet prove long-window throughput over many nonce TTL cycles.

The main latency split is clear:

- Cold Chutes evidence and verification is very slow: roughly 138-145 seconds
  in the latest probes.
- Warmed encrypted invocation is fast: roughly 1 second for the small probe
  prompts.

So Chutes evidence verification must stay off the user request path in normal
operation.

Non-Chutes live checks also passed:

- Tinfoil verified `kimi-k2-6`, returned one TLS SPKI channel binding, served a
  real chat completion, and the receipt/report verification example accepted
  the full response chain.
- NEAR AI verified `google/gemma-4-31B-it`, returned one TLS SPKI channel
  binding, served a real chat completion, and the receipt/report verification
  example accepted the full response chain.

These providers do not have a separate provider session lease in the current
implementation. Their lease is the cached verification result plus channel
binding. The backend enforces the TLS binding against the actual upstream HTTPS
connection before sending the request.

## Terms

This implementation has two different kinds of cached state.

Verification lease:

The cached result of verifying an upstream workload identity and its channel
binding. A valid verification lease says, "this upstream identity and binding
were verified under this provider adapter's rules until the verifier cache
expires." It never authorizes a different transport key.

Provider session lease:

Provider-specific material needed to send requests after the identity has been
verified. For Chutes this is a pool of single-use invocation nonces, each tied
to an instance id and E2EE public-key digest. A session lease is only usable
when it matches a currently verified channel binding.

Tinfoil and NEAR AI currently have no provider session lease. After
verification, each request creates a normal HTTPS connection and enforces the
verified TLS binding on that connection.

The session lease is subordinate to the verification lease. Session material
cannot extend trust after verification expires.

Attested session record:

A separate, read-only audit artifact written when a verified upstream event is
recorded (`record_attested_upstream_session`). It content-addresses the verified
binding, verifier id, target, and evidence digest into a stable `session_id`, stores
the matching `AttestedSessionRecord`, and attaches that id to the receipt. A relying
party fetches it back from `/v1/aci/sessions/{session_id}` and confirms the record's
target, verifier id, evidence digest, and channel bindings match the receipt event.

Its `expires_at` is deliberately the receipt TTL, not the verification lease TTL. The
record is a per-receipt historical attestation, so it must stay resolvable for as long
as the receipt that cites it; expiring it with the ~300 s lease would strand
`session_id`s in still-valid receipts. The record is not a claim that the binding is
still live now — `established_at` records when it was verified, and the forwarding path
only ever uses a binding from a fresh verification lease.

## Current No-Middleware Lifecycle

The implementation today is equivalent to the framework's middleware-disabled
mode: the public frontend and provider backend are one request path in the same
process. The client `body.model` is both the user-facing model and the target
route id.

Startup:

1. Load the single upstream config file.
2. Build a provider backend and verifier per configured upstream.
3. Prewarm upstream verification in the background.
4. Provider verifiers may record provider session material during prewarm.

Request path:

1. Treat the public model id as the target route id.
2. Rewrite the target route id to the upstream model id.
3. Verify the selected upstream, usually from a cached verification lease.
4. Refuse forwarding if verification is required and no verified binding exists.
5. Forward only through a backend that can enforce the verified binding.
6. Record the verified upstream event and request/response hashes in the
   receipt.

## Framework Lifecycle Target

The frontend/middleware/backend framework keeps the same lease semantics but
moves responsibility boundaries:

1. Frontend terminates downstream E2EE and records the user-facing request.
2. Optional middleware sees plaintext and may choose a target route id.
3. Backend validates the target route id, then runs the same verification lease
   and provider session lease path described here.
4. Backend records provider verification and provider-facing forwarding facts in
   shared request context.
5. Frontend finalizes the user-facing response and signs the receipt.

The verifier lease never belongs to middleware. Middleware may request a route;
backend decides whether that route is configured, verified, and enforceable.

Verification refresh:

The background refresh loop renews verification before cache expiry. The
default cadence is verifier cache TTL minus 60 seconds, so the normal 300
second cache refreshes every 240 seconds. If an upstream sets
`verification_refresh_seconds: 0`, it is skipped by the proactive refresh loop.
When multiple positive intervals exist, the current loop wakes at the shortest
active interval.

Refresh is non-destructive: a failed refresh does not delete the previous good
verification lease. User traffic can continue using the old verified identity
until that cache entry expires.

Provider session refresh:

For Chutes, the default session refresh interval is 45 seconds. Refresh uses
the cached verified binding to fetch a fresh `/e2e/instances` batch and records
only nonces whose instance key matches the verified binding. If Chutes returns
only keys outside the verified set, the refresh asks the verifier to refresh
evidence and widen the accepted key set.

Chutes nonce use:

1. Resolve model id to chute id, cached for 300 seconds.
2. Pop one unexpired nonce whose instance id and E2EE public-key digest match
   the verified binding.
3. If none exists, fetch `/e2e/instances`, filter by the verified binding, and
   record matching nonces.
4. Encrypt the OpenAI request body with Chutes ML-KEM-768 + HKDF-SHA256 +
   ChaCha20-Poly1305 and send `/e2e/invoke`.
5. Decrypt the buffered or streaming Chutes response before normal ACI receipt
   hashing.

The nonce pool uses the provider-reported `nonce_expires_in` when present,
otherwise it uses a local 55 second TTL. Expired nonces are discarded. Nonces
are single-use locally: popping a nonce removes it from the usable pool.

## Design Review

The good parts:

- Verification and transport enforcement are separated cleanly. The verifier
  decides what identity and binding to trust; the backend must prove it can
  enforce that binding before forwarding.
- Chutes is handled as a provider adapter, not forced into ACI/DCAP. That keeps
  ACI clean and makes provider adoption practical.
- Config is the operator-owned source of truth for provider credentials and
  provider-specific tuning. Verifier commands are not user-configured.
- Request forwarding is fail-closed when verification is required.
- Background refresh keeps cold provider evidence latency off the normal
  request path.

The current compromises:

- Verification refresh scheduling is coarse. The loop wakes at the shortest
  active configured interval, while individual targets opt in or out. This is
  simple, but not a precise per-upstream scheduler.
- Chutes session refresh is only as good as the intersection between the
  verified E2EE key set and Chutes' sampled `/e2e/instances` response. Multiple
  discovery rounds improve this, but do not make the provider's sampling
  deterministic.
- Chutes verification can record fresh nonces during verifier refresh, but the
  session refresh result currently reports `refreshed_nonces: 0` for the
  `refreshed_via_verifier` path. That is an instrumentation gap.
- All provider sessions are in memory. Restarting the gateway loses warmed
  nonces and model-id cache, which is acceptable for now because those leases
  are short-lived.
- We have short-window throughput lower bounds, not a long-window ceiling.

## Provider verification soundness

A soundness pass (2026-06) tamper-tested each provider's attestation verification
against the live upstream APIs. Per-provider "how it is verified and bound" references
live in [`providers/`](providers/README.md). Findings and the resulting state:

- **NEAR AI (TDX):** the quote is verified by the dstack verifier, but the
  `report_data` binding was being skipped (the check was gated on a field the dstack
  verifier never returns), so a wrong nonce or a swapped TLS fingerprint still
  verified. Fixed: `report_data` is now parsed from the verified quote and the nonce
  + signing-address + TLS-SPKI binding is enforced (fail-closed).
- **Chutes (TDX):** sound. The quote signature is verified with `dcap_qvl` (real DCAP
  collateral; `UpToDate` required) and `report_data` binds `SHA256(nonce‖e2e_pubkey)`.
- **Tinfoil (SEV-SNP, router mode):** the previous hand-rolled `_verify_snp` did **no**
  AMD signature verification — it only compared the measurement to a public Sigstore
  value, so a forged report with any `report_data` (TLS-SPKI) passed. Replaced with
  Tinfoil's official Python verifier (`tinfoil` SDK), which performs the full
  reference chain: AMD report signature + VCEK→ASK→ARK certificate chain and policy,
  Sigstore-verified code-measurement provenance bound to the GitHub repo/workflow
  identity, and the TLS public-key binding. The enforced binding value
  (`report_data[0:32]`) is unchanged; it is now cryptographically proven. Tamper tests
  confirm a modified `report_data`, measurement, or signature are all rejected.
- **NVIDIA GPU (NRAS), all providers:** tokens are fetched online from NRAS over TLS
  and the request nonce is checked (Chutes via `eat_nonce`, NEAR AI via the component
  nonce). The JWT signature is not additionally verified against NRAS' JWKS — a
  defense-in-depth follow-up, not a forgeable hole.

The principle: lean on the hardware/vendor reference verifier (Intel DCAP via
`dcap_qvl`, AMD via the `tinfoil`/`go-sev-guest` chain, NVIDIA via NRAS) rather than
re-implementing attestation crypto, and always bind the result to the transport.

## Next Measurements

To understand Chutes throughput better, run a long warmed test across several
nonce TTLs:

```bash
python3 scripts/live_e2e/chutes_rate_probe.py \
  --providers-file /tmp/chutes-gemma-provider.json \
  --provider chutes \
  --stage 120@0.5 \
  --stage 180@0.333 \
  --burst-concurrency 20 \
  --warmup 1 \
  --no-build \
  --keep-going-after-429 \
  --port 0
```

The signal to watch is not just 429s. Also check whether background session
refresh keeps the nonce pool healthy after the first minute and whether any
request falls back into slow evidence refresh.

## Open Questions

- P0: preserve this lifecycle while splitting the implementation into the
  frontend/middleware/backend framework documented in
  [frontend-middleware-backend.md](frontend-middleware-backend.md). The
  middleware-disabled path must remain behavior-compatible with the current
  request path.
- P0: finish strict-release pins from the provider reports under
  [providers/](providers/README.md): NEAR AI gateway
  provenance/runtime policy, Tinfoil router compose/image identity, and Chutes
  exact model-to-chute resolution. The first-pass soundness reviews are saved,
  but these provider-specific TODOs still gate general production inclusion.
- Should verification refresh become a real per-upstream scheduler before we
  add more providers?
- Should Chutes expose pool metrics: verified key count, pooled nonce count,
  nonce refresh success/failure, and channel-binding mismatch count?
- Should `refreshed_via_verifier` report the number of nonces recorded by the
  verifier bridge?
- Should we maintain a small proactive low-watermark refresh in addition to the
  fixed 45 second session refresh?
- What sustained Chutes request rate can the current account/model support over
  10-15 minutes without nonce starvation or provider-side 429s?
