# Attested Sessions: Implementation Notes

Attested sessions — immutable, content-addressed records of verified upstream
TEE channels — are specified in [spec/aci.md](../spec/aci.md) §9 (record
shape, session ids, endpoints, retention) and §9.3 (the typed claim
vocabulary). This note covers only what the spec leaves to the
implementation: how this gateway stores sessions, how each provider adapter
maps its evidence onto the typed claims, and why the preflight survey exists.

The types live in `src/aggregator/session.rs`; the store in
`src/aggregator/session_store.rs`.

## Lifecycle in this gateway

Sealing a session is pure attestation: the verification fetches and checks
the provider's attestation (the TEE quote, the pinned TLS public key / SPKI,
the signing key) and serializes the verified material plus typed claims into
the session document once. Those bytes are stored and always served
byte-identically; the session id is their SHA-256 (spec §9). It is never a
model call — no prompt, no inference, none of the user's data.

Background upstream verification establishes and refreshes sessions before
traffic; request completion records the session actually served on the
receipt's `upstream.verified` event. Both paths write through the same
process-owned store. The session's validity period reuses
`receipt_ttl_seconds`, and retention extends per citing receipt, satisfying
the §9 rule that a session outlives every receipt citing it.

Full trace:

```
request → receipt (x-receipt-id)
        → upstream.verified { session_id }
        → session record { identity, channel_binding, claims, evidence }
```

## Preflight survey

`GET /v1/aci/sessions?upstream_name=&model=` is a read of the same store: a
user can inspect the verified identity, channel binding, and typed claims —
and check the pinned public key / SPKI — for a model *before* releasing any
data. The forwarding path never trusts a stored session for freshness; it
only forwards on a fresh verification lease (see
[upstream-verification-lifecycle.md](upstream-verification-lifecycle.md)).

## Storage: compacted JSONL

The durable store appends records to `sessions.jsonl` in the gateway state
directory and replays them into an in-memory index on startup. Record
integrity comes from recomputing the content-addressed session id over the
stored bytes; receipt signatures link requests to those ids. At-rest
durability and confidentiality remain deployment concerns.

The gateway takes an advisory lock on a separate `sessions.jsonl.lock` file
so only one process can own the log. On startup and hourly thereafter it
rewrites the live index through a synced temporary file and atomic rename,
dropping duplicate, expired, malformed, or truncated history.

## Per-provider claim mapping

`session_claims_for_event` maps a verified upstream event onto the typed
claims honestly: a claim is asserted only when *this* verifier's evidence
backs it, and the raw provider facts are preserved verbatim in `claims.extra`
so a deep auditor sees the full provider scope. The key names inside `extra`
are a stable contract (spec §9.3) — consumers may depend on them. The event
carries a stable `provider_type` (distinct from the operator's per-endpoint
config `upstream_name`) that selects the mapping. A `failed` result asserts
nothing.

| Claim | tinfoil | near-ai | chutes | phala-direct | generic |
| --- | --- | --- | --- | --- | --- |
| `tee_attested` | ✅ hardware | ✅ hardware | ✅ hardware | ✅ hardware | ✅ verifier-derived |
| `tcb_up_to_date` | tri-state¹ | tri-state¹ | tri-state¹ | tri-state¹ | unknown |
| `serving_software_known_good` | ✅ Sigstore² | unknown | unknown | unknown | unknown |
| `os_known_good` | unknown | unknown | unknown | unknown | unknown |
| `gpu_attested` | unknown | unknown | ✅³ | ✅³ | unknown |
| `model_weights_provenance` | unknown | unknown | unknown | unknown | unknown |

- For Tinfoil, NEAR AI, Chutes, and PhalaDirect, `tee_attested` is
  `hardware_proven`: a genuine TEE quote was verified and the request channel
  bound to it. For NEAR AI this is the **gateway** TD — a router that fronts
  many models behind one TEE, so its attested session is the gateway
  *channel*: one session per router, not per model, with the served model
  recorded on the receipt. The verifier attests exactly that channel — its
  `AttestationScope` is `PerRouter`, enforced fail-closed at the binding
  seam. Per-model TEE coverage is delegated to the verified gateway, which
  verifies its backend model TDs before serving them; because the gateway's
  own integrity and source provenance are verified, that delegation is sound
  without re-verifying each backend quote here. The remaining roadmap item is
  finer: binding the exact backend instance to a specific request (see
  [roadmap.md](roadmap.md)).
- ¹ `tcb_up_to_date` is an honest tri-state from the verifier's reported
  `tcb_status` (`hardware_proven`): `UpToDate` asserts, any other reported
  status **refutes** (the quote proves a stale TCB — the gateway records the
  bad claim but does **not** hard-reject the session), and an absent status
  is `unknown`. Freshness is never asserted by policy. All four provider
  verifiers surface `tcb_status`: NEAR AI and Phala-direct read it from the
  dstack verifier, which reports TCB freshness separately from its overall
  `is_valid`, so a stale TCB shows up without failing the gateway; Chutes
  records the per-instance and fleet-aggregated status, so an OutOfDate
  instance serves with a refuted claim (quote signature, report-data binding,
  debug bit and measurement match stay hard gates); Tinfoil's official
  verifier owns a fail-closed TCB gate with no separable status, so a
  verified result reports `UpToDate`.
- ² Tinfoil compares its SEV-SNP launch measurement against the Sigstore
  golden values published for the build's repo; the reason cites
  `config_repo` / `release_digest`. Source is `verifier_derived`.
- ³ `gpu_attested` asserts (`verifier_derived`) when the provider's NVIDIA
  confidential-computing GPU attestation is verified *and* nonce-bound
  (Chutes and Phala-direct surface it; NEAR AI / Tinfoil do not). It attests
  a genuine CC GPU, **not** its binding to the serving CPU TEE — hence
  `verifier_derived`, not `hardware_proven` (spec §9.3 states this limit) —
  and it never gates a session. Absent or unverified GPU evidence leaves it
  `unknown` (never a refutation on an ambiguous negative). The raw
  `gpu_verified` / `gpu_arch` facts also stay in `extra`.
- "generic" is a verifier path with no provider-specific identity: it asserts
  only `tee_attested` (`verifier_derived`), nothing else.

## Source-code provenance is verifier-owned

Source-code-level verification — that a measured image/compose maps to
reviewed source — is owned by the verifier, not modeled by a gateway schema.
The verifier decides how it establishes provenance (matching known
measurements, a pinned image digest, a signed SLSA/in-toto attestation, a
reproducible build, …) and returns the result as the
`serving_software_known_good` / `os_known_good` claims with status, source,
and reason. The gateway records and surfaces these verbatim. Adding stronger
provenance methods later is a change inside a verifier, not a change to the
session model or config.

## Configuration

Config is thin: it says *what to connect to*, not *what is trusted*. One
provider entry holds many models; each `models` value is either a plain
`upstream_model_id` string (inherits the provider `base_url`) or an object
adding a per-model `endpoint` for providers where each model is its own TLS
endpoint (driving case: direct dstack-vllm-proxy GPU workers). The channel
binding (TLS SPKI / provider E2EE key) and every claim are supplied by the
verifier dynamically — config carries no SPKI pin, no provenance pins, and no
asserted claims.

## References

- [spec/aci.md](../spec/aci.md) §9 — the session record, ids, endpoints,
  retention; §9.3 — the claim vocabulary; §10.3 — the audit procedure.
- [providers/audit-criteria.md](providers/audit-criteria.md) — the criteria
  behind the claim model.
- [upstream-verification-lifecycle.md](upstream-verification-lifecycle.md) —
  lease vs session-record semantics.
