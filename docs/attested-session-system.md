# Attested Sessions: Implementation Notes

Attested sessions — immutable, content-addressed records of verified upstream
TEE channels — are specified in [spec/aci.md](../spec/aci.md) §8 (record
shape, session ids, endpoints, retention) and §8.3 (the typed claim
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
byte-identically; the session id is their SHA-256 (spec §8). It is never a
model call — no prompt, no inference, none of the user's data.

Background upstream verification establishes and refreshes sessions before
traffic; request completion records the session actually served on the
receipt's `upstream.verified` event. Both paths write through the same
process-owned store. The session's validity period reuses
`receipt_ttl_seconds`, and retention extends per citing receipt, satisfying
the §8 rule that a session outlives every receipt citing it.

Full trace:

```
request → receipt (x-receipt-id)
        → upstream.verified { session_id }
        → AttestedSession { claims (+ reasons), channel_binding, evidence }
```

## Preflight survey

`GET /v1/aci/sessions?upstream_name=&model=` is a read of the same store: a
user can inspect the verified identity, channel binding, and typed claims —
and check the pinned public key / SPKI — for a model *before* releasing any
data. The forwarding path never trusts a stored session for freshness; it
only forwards on a fresh verification lease.

## Storage: compacted JSONL

The durable session store appends typed records
(`{ seq, ts, type, payload }`) to `sessions.jsonl` and replays them into an
in-memory index on startup. Record integrity comes from recomputing the
content-addressed `session_id`; receipt signatures link requests to those
session ids. At-rest durability and confidentiality remain deployment
concerns.

The gateway takes an advisory lock on a separate `sessions.jsonl.lock` file so
only one process can own the log. On startup and hourly thereafter it rewrites
the live, non-expired index through a synced temporary file and atomic rename,
dropping duplicate, expired, malformed, or truncated history.

## Per-provider claim mapping

`session_claims_for_event` maps a verified upstream event onto the typed
claims honestly: a claim is asserted only when *this* verifier's evidence
backs it, and the raw provider facts are preserved verbatim in `claims.extra`
so a deep auditor sees the full provider scope. The key names inside `extra`
are a stable contract (spec §8.3) — consumers may depend on them. The event
carries a stable `provider_type` (distinct from the operator's per-endpoint
config `name`) that selects the mapping. A `failed` result asserts
nothing.

| Claim | tinfoil | near-ai | chutes | phala-direct | secret-ai⁴ | generic |
| --- | --- | --- | --- | --- | --- | --- |
| `tee_attested` | ✅ hardware | ✅ hardware | ✅ hardware | ✅ hardware | ✅ hardware | ✅ verifier-derived |
| `tcb_up_to_date` | tri-state¹ | tri-state¹ | tri-state¹ | tri-state¹ | TDX ✅ / SEV unknown | unknown |
| `serving_software_known_good` | ✅ Sigstore² | unknown | unknown | unknown | optional pin | unknown |
| `os_known_good` | unknown | unknown | unknown | unknown | ✅ registry | unknown |
| `gpu_attested` | unknown | unknown | ✅³ | ✅³ | ✅ required | unknown |
| `model_weights_provenance` | unknown | unknown | unknown | unknown | unknown | unknown |

- For the five real provider verifiers `tee_attested` is `hardware_proven`: a
  genuine TEE quote was verified and the request channel bound to it.
- NEAR AI's quote covers its **gateway** TD, a router fronting many models
  behind one TEE. Its attested session is that gateway *channel* — one session
  per router, not per model — with the served model recorded on the receipt.
  The verifier attests exactly that channel (`AttestationScope::PerRouter`,
  enforced fail-closed at the binding seam). The verified gateway checks its
  backend model TDs before serving them; since the gateway's own integrity and
  source provenance are verified, that delegation is sound without
  re-verifying each backend quote here. Still open: binding the exact backend
  instance to a specific request ([roadmap.md](roadmap.md)).
- ¹ `tcb_up_to_date` is a tri-state read from the verifier's reported
  `tcb_status` (`hardware_proven`). `UpToDate` asserts. Any other reported
  status **refutes**: the quote proves a stale TCB, which the gateway records
  without hard-rejecting the session. An absent status is `unknown`, and
  freshness is never asserted by policy. Per provider:
  - NEAR AI and Phala-direct read `tcb_status` from the dstack verifier, which
    reports freshness separately from its overall `is_valid`, so a stale TCB
    shows up without failing the gateway.
  - Chutes records the per-instance and fleet-aggregated status, so an
    `OutOfDate` instance serves with a refuted claim. Quote signature,
    report-data binding, debug bit, and measurement match stay hard gates.
  - Tinfoil's official verifier has a fail-closed TCB gate with no separable
    status, so a verified result reports `UpToDate`.
- ² Tinfoil compares its SEV-SNP launch measurement against the Sigstore golden
  values published for the build's repo; the reason cites `config_repo` /
  `release_digest`. Source is `verifier_derived`.
- ³ `gpu_attested` asserts (`verifier_derived`) when the provider's NVIDIA
  confidential-computing GPU attestation is verified *and* nonce-bound. It
  attests a genuine CC GPU, **not** its binding to the serving CPU TEE — spec
  §8.3 states this limit. Whether failed GPU evidence rejects verification is
  provider policy: SecretAI requires it; Chutes and Phala-direct keep it
  supplemental. Absent or unverified evidence leaves the claim `unknown`,
  never refuted on an ambiguous negative. The raw `gpu_verified` / `gpu_arch`
  facts stay in `extra`.
- ⁴ SecretAI policy and claim details are documented in
  [SecretAI verification](providers/secret-ai/verification.md).
- "generic" is a verifier path with no provider-specific identity: it asserts
  only `tee_attested` (`verifier_derived`), nothing else.

## Source-code provenance

Source-code-level verification — that a measured image/compose maps to reviewed
source — is **owned by the verifier**, not modeled by a gateway schema. The
verifier decides how it establishes provenance (matching hard-coded known
measurements, a pinned image digest, a signed SLSA/in-toto attestation, a reproducible build, …) and returns the result as the
`serving_software_known_good` / `os_known_good` claims with:

- `status` (asserted / refuted / unknown),
- `source` (e.g. `verifier_derived`),
- `reason` (e.g. `"compose hash matches reviewed image X"` or
  `"hard-coded known measurements"`),
- optional `evidence_ref`.

The gateway records and surfaces these verbatim. Adding stronger provenance
methods later is a change inside a verifier, not a change to the session model
or config.

## Configuration

Config is thin by default. Optional provider policy (`accepted_subjects`,
`accepted_image_digests`, `accepted_dstack_kms_root_public_keys`) narrows
accepted identities; it never supplies claims. One provider entry holds many
models, and each `models` value is a plain `upstream_model_id` string
inheriting the provider `base_url` (a per-model `endpoint` form is still
open). The channel binding and every claim come from the verifier at
verification time — config never carries a raw SPKI pin or an asserted
claim.

## References

- [spec/aci.md](../spec/aci.md) §8 — the session record, ids, endpoints,
  retention; §8.3 — the claim vocabulary; §9.2 — session verification.
- [providers/audit-criteria.md](providers/audit-criteria.md) — the criteria
  behind the claim model.
- [upstream-verification-lifecycle.md](upstream-verification-lifecycle.md) —
  lease vs session-record semantics.
