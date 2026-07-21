# Attested Session System

Date: 2026-06-10 UTC.
Status: design, in progress. Refines the per-request audit record described
under "Attested session record" in
[upstream-verification-lifecycle.md](upstream-verification-lifecycle.md).

This note specifies attested sessions as **immutable, content-addressed,
provider-owned** records: one provider owns one session per verified TEE channel
(its endpoint, with a new one whenever the verified material changes), each
carrying a typed claim set and an enforceable channel binding, persisted so a
receipt can always be traced back to the exact security context that served it.

## Motivation

Today an attested session is a per-request audit record
(`AttestedSessionRecord`, `src/aggregator/service.rs`): it is content-addressed
from the `UpstreamVerifiedEvent`, stored in the in-memory `ReceiptStore.sessions`
map, and retained for `receipt_ttl_seconds`. Three things are missing:

1. **Importing many sessions per provider.** A provider hosts many models, each
   potentially its own endpoint with its own TLS certificate. We want one
   provider to own *N* sessions, one per model-endpoint. (Driving case: direct
   dstack-vllm-proxy GPU workers, where each model is a distinct endpoint.)
2. **Typed, honest claims.** `verification.provider_claims: Value` plus a derived
   `verified_claims: Vec<String>` carry no fixed vocabulary, no
   present/absent/unknown distinction, and no record of *who vouches* for each
   claim. "TCB up to date" proven by collateral and "model weights provenance
   good" vouched by the operator must not look alike.
3. **Persistence.** The store is in memory; a restart loses the audit trail.

## Principle: sessions are immutable

A session captures **one** verified state — identity, channel binding, claims,
and evidence, verified at a point in time. It is never mutated. Its id is
content-addressed over that material, so:

- Identical verified material re-verifies to the **same** session id (idempotent
  dedup — re-verifying the same endpoint with the same result does not multiply
  records).
- **Any** change in the verified material (a rotated TLS SPKI on cert renewal, a
  new measurement on redeploy, a changed claim) produces a **different** id —
  i.e. a new, separate session. The security context can never silently change
  under a fixed id.
- A receipt references the exact session id it used. Resolving that id returns
  the precise, unchanging security context for that request.

"One provider owns many sessions" follows naturally: many distinct TEE channels
(endpoints), plus a new session each time a channel's verified material changes.
A router-based upstream that fronts many models behind one TEE is a single
channel — one session — and the model served is recorded on the receipt.

## Goals and non-goals

Goals:

- An immutable, content-addressed session per verified state, referenced by the
  receipts that used it, persisted durably.
- A typed claim set that is honest about each claim's *source* and carries a
  verifier-supplied *reason*; missing claims are `unknown` (transparency, never
  a silent pass).
- File-backed, tamper-evident persistence that can later feed a public
  transparency log.

Non-goals (kept out deliberately):

- No mutable sessions, epochs, or in-place refresh of a session's material.
- No `Direction`/downstream sessions, no session revocation/status machine —
  not used today.
- **No gateway-defined provenance schema.** Source-code-level verification is
  the verifier's responsibility; the gateway records the claim and reason it
  returns (see "Source-code provenance").
- No channel binding or asserted claims in config: those come from the verifier.
  Provider policy may pin accepted verified identities.
- No policy DSL. The fail-closed gate stays on *verification result +
  enforceable binding*; claims are a transparency surface.

## Data model

A session is the verified **TEE channel** — the attested remote service a
request binds to — identified by its endpoint + channel binding + evidence, not
by model. A router-based upstream that serves many models behind one TEE
therefore yields **one** session (no per-model duplication); the model served is
recorded on the receipt's `upstream.verified` event, not on the session.

```rust
/// One immutable, verified TEE channel. Content-addressed; never mutated.
struct AttestedSession {
    api_version: String,
    session_id: String,            // "as_" + sha256 over the verified material below
    upstream_name: String,         // the operator's upstream config name this channel belongs to
    endpoint: Option<String>,      // the verified upstream origin
    verifier_id: String,
    established_at: u64,            // when this material was verified
    expires_at: u64,               // retention deadline (>= the TTL of citing receipts)
    identity: Option<WorkloadIdentity>, // verified identity keys (e.g. signing_address)
    channel_binding: Vec<ChannelBinding>, // enforceable binding(s); reuses src/aci/receipt.rs
    claims: SessionClaims,
    evidence: EvidenceRef,         // common evidence object (digest + data-uri)
}
```

`session_id` is `"as_" + hex(sha256(JCS(material)))` where `material` is the
immutable subset — upstream_name, endpoint, verifier_id, identity, channel
binding, claims, and the evidence digest (no model: the channel, not the model,
is what is attested). Timestamps are excluded so identical material dedups to one
id.
`established_at` records when it was verified; `expires_at` is a *retention*
window (kept at least as long as the receipts that cite it), not a
binding-validity deadline — the forwarding path only ever uses a binding from a
fresh verification lease.

## Typed claims

A fixed vocabulary mapped to the audit criteria. Each claim is a tri-state plus
an explicit source and a verifier-supplied reason. Missing ⇒ `Unknown`.

```rust
enum ClaimStatus { Asserted, Refuted, Unknown }

/// Who vouches for the claim — sets the assurance level honestly.
enum ClaimSource {
    HardwareProven,   // from the verified quote/collateral itself
    VerifierDerived,  // computed by the verifier from verified evidence
    ProviderAsserted, // published by the provider, not independently proven
    OperatorAsserted, // declared by the gateway operator
}

struct Claim {
    status: ClaimStatus,
    source: Option<ClaimSource>,   // Some only when Asserted/Refuted
    reason: Option<String>,        // verifier's plain reason, e.g. "matches hard-coded known measurements"
    evidence_ref: Option<String>,  // pointer into the evidence backing the claim
}

struct SessionClaims {
    tee_attested: Claim,                 // §1  genuine CPU TEE, identity bound
    gpu_attested: Claim,                 // GPU is good — see note
    tcb_up_to_date: Claim,               // §14 platform TCB freshness
    os_known_good: Claim,                // §13 platform/OS provenance
    serving_software_known_good: Claim,  // §13 software provenance (verifier-asserted)
    model_weights_provenance: Claim,     // §4  served weights / quant honesty
    extra: BTreeMap<String, Claim>,      // provider-owned scope facts
}
```

The verifier *asserts* each claim and supplies the reason; the gateway records
and surfaces it. The gateway does not compute provenance itself.

**GPU attestation.** `gpu_attested` asserts (`VerifierDerived`) when the
provider's NVIDIA confidential-computing GPU attestation is verified **and
nonce-bound** to this verification round — a genuine CC GPU, cryptographically
checked. It is deliberately *not* `HardwareProven`, and the reason is explicit
that this attests the GPU itself, **not** its binding to the serving CPU TEE:
an NRAS check proves a CC-capable GPU *exists* for a nonce, not that it serves
this request or is bound to the CPU quote. Proving *that* needs the reviewed
serving software (measured into the CPU TEE quote) to locally attest the GPU and
set up the encrypted CPU↔GPU channel — a stronger statement we do not make here.
Whether failed GPU evidence rejects verification is provider policy. Absent or
unverified supplemental GPU evidence leaves the claim `Unknown`.

## Source-code provenance

Source-code-level verification — that a measured image/compose maps to reviewed
source — is **owned by the verifier**, not modeled by a gateway schema. The
verifier decides how it establishes provenance (matching hard-coded known
measurements, a pinned image digest, a signed SLSA/in-toto attestation in a
transparency log, a reproducible build, …) and returns the result as the
`serving_software_known_good` / `os_known_good` claims with:

- `status` (asserted / refuted / unknown),
- `source` (e.g. `VerifierDerived`),
- `reason` (e.g. `"compose hash matches reviewed image X"` or
  `"hard-coded known measurements"`),
- optional `evidence_ref`.

The gateway records and surfaces these verbatim. Adding stronger provenance
methods later is a change inside a verifier, not a change to the session model
or config.

## Per-provider claim mapping

`session_claims_for_event` maps a verified upstream event onto the typed claims
**honestly**: a claim is asserted only when *this* verifier's evidence backs it,
and the raw `provider_claims` are always preserved verbatim in `extra` so a deep
auditor sees the full provider scope. The event carries a stable `provider_type`
(distinct from the operator's per-endpoint config `name`) that selects the
mapping. A `failed` result asserts nothing.

| Claim | tinfoil | near-ai | chutes | phala-direct | secret-ai⁴ | generic |
| --- | --- | --- | --- | --- | --- | --- |
| `tee_attested` | ✅ hardware | ✅ hardware | ✅ hardware | ✅ hardware | ✅ hardware | ✅ verifier-derived |
| `tcb_up_to_date` | tri-state¹ | tri-state¹ | tri-state¹ | tri-state¹ | TDX ✅ / SEV unknown | unknown |
| `serving_software_known_good` | ✅ Sigstore² | unknown | unknown | unknown | optional pin | unknown |
| `os_known_good` | unknown | unknown | unknown | unknown | ✅ registry | unknown |
| `gpu_attested` | unknown | unknown | ✅³ | ✅³ | ✅ required | unknown |
| `model_weights_provenance` | unknown | unknown | unknown | unknown | unknown | unknown |

- For the five real provider verifiers `tee_attested` is `HardwareProven`: a
  genuine TEE quote was verified and the request channel bound to it. For NEAR AI
  this is the **gateway** TD — a router that fronts many models behind one TEE,
  so its attested session is the gateway *channel*: one session per router, not
  per model, with the served model recorded on the receipt. The verifier attests
  exactly that channel — its `AttestationScope` is `PerRouter`, enforced
  fail-closed at the binding seam. Per-model TEE coverage is delegated to the
  verified gateway, which verifies its backend model TDs before serving them;
  because the gateway's own integrity and source provenance are verified, that
  delegation is sound without re-verifying each backend quote here. The remaining
  roadmap item is finer: binding the exact backend instance to a specific request
  (a per-instance, request-bound model attestation on the receipt — see
  [roadmap.md](roadmap.md)).
- ¹ `tcb_up_to_date` is an honest tri-state from the verifier's reported
  `tcb_status` (`HardwareProven`): `UpToDate` asserts, any other reported status
  **refutes** (the quote proves a stale TCB — the gateway records the bad claim
  but does **not** hard-reject the session), and an absent status is `unknown`.
  Freshness is never asserted by policy. NEAR AI, Chutes, and Phala-direct
  surface `tcb_status`: NEAR AI and Phala-direct read it from the dstack
  verifier, which reports TCB freshness separately from its overall `is_valid`,
  so a stale TCB
  shows up without failing the gateway; Chutes no longer hard-rejects a stale
  TCB — it records the per-instance and fleet-aggregated status, so an OutOfDate
  instance serves with a refuted claim (quote signature, report-data binding,
  debug bit and measurement match stay hard gates); Tinfoil's official verifier
  owns a fail-closed TCB gate with no separable status, so a verified result
  reports `UpToDate`.
- ² Tinfoil compares its SEV-SNP launch measurement against the Sigstore golden
  values published for the build's repo; the reason cites `config_repo` /
  `release_digest`. Source is `VerifierDerived`.
- ³ `gpu_attested` asserts (`VerifierDerived`) when the provider's NVIDIA
  confidential-computing GPU attestation is verified *and* nonce-bound (Chutes
  and Phala-direct surface it; NEAR AI / Tinfoil do not). It attests a genuine CC
  GPU, **not** its binding to the serving CPU TEE — hence `VerifierDerived`, not
  `HardwareProven` (see the GPU note above) — and it never gates a session.
  Absent or unverified GPU evidence leaves it `unknown` (we do not refute on an
  ambiguous negative). The raw `gpu_verified` / `gpu_arch` facts also stay in
  `extra`.
- ⁴ SecretAI policy and claim details are documented in
  [SecretAI verification](providers/secret-ai/verification.md).
- "generic" is a verifier path with no provider-specific identity: it asserts
  only `tee_attested` (`VerifierDerived`), nothing else.

## Configuration

Config is thin by default. Optional provider policy can narrow accepted
identities without supplying asserted claims. One provider entry holds many
models; each value is either the legacy `String`
(`upstream_model_id`, inherits the provider `base_url`) or an object:

```jsonc
{
  "name": "phala-direct",
  "provider": "phala-direct",
  "bearer_token": "…",
  "models": {
    "glm51-phala": {
      "upstream_model_id": "zai-org/GLM-5.1",
      "endpoint": "https://node-7.example.net"  // per-model endpoint (own TLS cert)
    }
  }
}
```

When a model omits `endpoint` it inherits the provider `base_url` (one endpoint
serving all of a provider's models). When each model is its own endpoint, the
loader builds one verifier + route + session per `(model, endpoint)`.

The channel binding and claims are supplied dynamically by the verifier. Config
may carry provider policy such as SecretAI workload pins, but no raw SPKI or
asserted claims.

## Receipt linkage

The `upstream.verified` receipt event already carries `session_id`
(`add_upstream_verified_with_session`). Full trace:

```
request → receipt (x-receipt-id)
        → upstream.verified { session_id }
        → AttestedSession { claims (+ reasons), channel_binding, evidence }
```

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

## API surface

All ACI verification artifacts live under `/v1/aci/` so they do not pollute the
OpenAI surface. Every artifact and gateway envelope carries one umbrella
API-version token, `aci/1`: in-body as `api_version` on signed artifacts (so the
version lives inside the signed bytes, where a header cannot), and as the
`X-ACI-Version` response header on every HTTP response (stamped by a single
middleware, error paths included). This is a separate axis from the
`aci.<purpose>.v1` strings (`aci.report_data.v1`, `aci.keyset.endorsement.v1`)
— those are cryptographic domain-separation tags for the two signed payloads
(the attestation report-data statement and the keyset endorsement), not API
versions, and version independently. Receipts carry no purpose tag: the
signature covers the whole canonical receipt, which is self-describing.

Canonical (clean shapes):

- `GET /v1/aci/attestation?nonce=` — the bare gateway attestation report
  (preflight identity / liveness).
- `GET /v1/aci/receipts/{id}` — the signed ACI receipt (canonical value). `id`
  is the gateway `receipt_id` (preferred) or upstream `chat_id`. The
  `upstream.verified` event carries the typed claim verdicts inline (shallow
  audit) plus the content-addressed `session_id`.
- `GET /v1/aci/sessions/{session_id}` — the immutable session record, with full
  evidence + per-claim reasons (deep audit).
- `GET /v1/aci/sessions?upstream_name=&model=` — a provider's attested sessions. This
  is the **preflight survey**: a read of the
  session store (see below), so a user can inspect the attested session + claims
  for a model *before* releasing any data.

### One store, one process owner

The in-memory session store is the serving source of truth. Background upstream
verification establishes and refreshes sessions before traffic, while request
completion persists the session actually served. Both paths write through the
same store instance; the separate lock file prevents another gateway process
from concurrently owning the JSONL log.

Sealing a session is **pure attestation**: the verification fetches and checks
the provider's attestation (the TEE quote, the pinned TLS public key / SPKI, the
signing key) and stores that verified material plus the typed claims. It is
**never** a model call — no prompt, no inference, none of the user's data.
Sessions are keyed on the configured upstream `model_id` and content-addressed,
so re-verifying an unchanged endpoint resolves to the same record (an idempotent
no-op), while a rotated key or changed measurement seals a new session and the
previous one ages out with its retention TTL.

Everything else just reads this store. The preflight API
(`GET /v1/aci/sessions?...`) returns the currently available sessions; the live
completion path references the session for the request it served by its
content-addressed `session_id` rather than copying one. A user reads the
preflight survey to see the verified identity, channel binding, and typed claims
— and check the pinned public key / SPKI — before deciding whether to release
their prompts.

No bundle and no `/body`: the artifacts are *linked, not bundled*. A receipt
references its session by content-addressed `session_id`; a verifier follows the
link to `/v1/aci/sessions/{id}` (immutable, cacheable, race-free). The gateway
never stores request bodies, so there is nothing to fetch — the rewrite is
committed by `request.forwarded.body_hash` + `transparency.request_modified`.

Legacy aliases — dstack-vllm-proxy paths only (no back-compat owed to earlier
private-ai-gateway paths):

- `GET /v1/attestation/report` — report plus legacy e2ee/`signing_address` fields.
- `GET /v1/signature/{id}` — the legacy signature wrapper
  (`text`/`signature`/`signing_address`) with the receipt in `receipt`.

## Implementation increments

1. **Immutable session + store.** `AttestedSession` / `SessionClaims` types; a
   session store trait; a content-addressed JSONL implementation with replay and
   periodic compaction; keep in-memory for tests. Rework
   `record_attested_upstream_session` to seal the immutable session and persist
   it. Receipts already cite `session_id`.
2. **Typed claims from verifiers.** Each provider adapter populates
   `SessionClaims` (status + source + reason) from its verified evidence —
   including `serving_software_known_good` / `os_known_good` as the
   source-provenance surface.
3. **One store, one process owner (preflight).** *Done.* Background upstream
   verification seals each verified result through an `UpstreamSessionSink`,
   while completion persists the session actually served. Both use the same
   process-owned store, so `/v1/aci/sessions` is a real preflight survey without
   a separate import loop. Still open: the per-model `endpoint` object
   config form (today each model maps to a single `upstream_model_id` inheriting
   the provider `base_url`).

The `/v1/aci/` namespace, the sessions-list endpoint, and the dropping of the
`/body` route are already in place.

## References

- [roadmap.md](roadmap.md) — P0 Attested Sessions and Audit Log.
- [providers/audit-criteria.md](providers/audit-criteria.md) — §1, §4, §7, §11,
  §13, §14 underpin the claim model.
- [upstream-verification-lifecycle.md](upstream-verification-lifecycle.md) —
  lease vs session-record semantics this builds on.
