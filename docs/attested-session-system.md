# Attested Session System

Date: 2026-06-10 UTC.
Status: design, in progress. Refines the per-request audit record described
under "Attested session record" in
[upstream-verification-lifecycle.md](upstream-verification-lifecycle.md).

This note specifies attested sessions as **immutable, content-addressed,
provider-owned** records: one provider imports many sessions (one per
model-endpoint, and a new one whenever the verified material changes), each
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

"One provider imports many sessions" follows naturally: many model-endpoints,
plus a new session each time a model-endpoint's verified material changes.

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
- No security material in config: the channel binding and all claims are
  supplied by the verifier at verify time, not pinned in config.
- No policy DSL. The fail-closed gate stays on *verification result +
  enforceable binding*; claims are a transparency surface.

## Data model

```rust
/// One immutable, verified session. Content-addressed; never mutated.
struct AttestedSession {
    api_version: String,
    session_id: String,            // "as_" + sha256 over the verified material below
    provider: String,              // e.g. "phala-direct"
    model_id: String,              // the model attested, as routed to the upstream
    upstream_model_id: Option<String>, // the upstream's own id, only when it differs
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
immutable subset — provider, public/upstream model id, endpoint, verifier_id,
identity, channel binding, claims, and the evidence digest. Timestamps are
excluded so identical material dedups to one id. `established_at` records when it
was verified; `expires_at` is a *retention* window (kept at least as long as the
receipts that cite it), not a binding-validity deadline — the forwarding path
only ever uses a binding from a fresh verification lease.

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

**GPU attestation.** `gpu_attested` is a real, assertable claim, but it is
established the sound way: from **CPU attestation + a software source-code
check** — the reviewed serving software measured into the CPU TEE quote is what
locally attests the GPU and sets up the encrypted CPU↔GPU channel, so a verifier
that has checked that software can explicitly assert "the GPU is good" (and may
include the hardware model). What is *not* sound is treating a standalone GPU
attestation as the basis: an NRAS token only proves a CC-capable GPU *exists*,
not that it serves this request or is bound to the serving CPU TEE, so the
gateway never gates on a gateway-side NRAS check (verifying its JWT against
NRAS' JWKS does not change that — it is an existence oracle, not a binding
proof). Source for `gpu_attested` is therefore `VerifierDerived` via the
CPU+software chain, never a raw GPU-token check.

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
auditor sees the full provider scope. The event carries a stable `provider` type
(distinct from the operator's per-endpoint config `vendor`) that selects the
mapping. A `failed` result asserts nothing.

| Claim | tinfoil | near-ai | chutes | phala-direct | generic |
| --- | --- | --- | --- | --- | --- |
| `tee_attested` | ✅ hardware | ✅ hardware | ✅ hardware | ✅ hardware | ✅ verifier-derived |
| `tcb_up_to_date` | tri-state¹ | tri-state¹ | tri-state¹ | tri-state¹ | unknown |
| `serving_software_known_good` | ✅ Sigstore² | unknown | unknown | unknown | unknown |
| `os_known_good` | unknown | unknown | unknown | unknown | unknown |
| `gpu_attested` | unknown³ | unknown³ | unknown³ | unknown³ | unknown |
| `model_weights_provenance` | unknown | unknown | unknown | unknown | unknown |

- For the four real provider verifiers `tee_attested` is `HardwareProven`: a
  genuine TEE quote was verified and the request channel bound to it.
- ¹ `tcb_up_to_date` is an honest tri-state from the verifier's reported
  `tcb_status` (`HardwareProven`): `UpToDate` asserts, any other reported status
  **refutes** (the quote proves a stale TCB — the gateway records the bad claim
  but does **not** hard-reject the session), and an absent status is `unknown`.
  Freshness is never asserted by policy. All four provider verifiers surface
  `tcb_status`: NEAR AI and Phala-direct read it from the dstack verifier, which
  reports TCB freshness separately from its overall `is_valid`, so a stale TCB
  shows up without failing the gateway; Chutes no longer hard-rejects a stale
  TCB — it records the per-instance and fleet-aggregated status, so an OutOfDate
  instance serves with a refuted claim (quote signature, report-data binding,
  debug bit and measurement match stay hard gates); Tinfoil's official verifier
  owns a fail-closed TCB gate with no separable status, so a verified result
  reports `UpToDate`.
- ² Tinfoil compares its SEV-SNP launch measurement against the Sigstore golden
  values published for the build's repo; the reason cites `config_repo` /
  `release_digest`. Source is `VerifierDerived`.
- ³ `gpu_attested` is never asserted from a GPU/NRAS token alone (see the GPU
  note above); the raw `gpu_verified` / `gpu_arch` facts remain in `extra`. The
  GPU check still runs — when it succeeds it authenticates the GPU model/info the
  CPU TEE quote does not itself vouch for — but it never gates a session: a real
  TEE GPU is already established by the measured serving software inside the
  quote, so a failed or absent GPU result is recorded as supplemental metadata,
  not a rejection (Chutes and Phala-direct both treat it this way).
- "generic" is the static / preverified / DCAP path with no provider identity:
  it asserts only `tee_attested` (`VerifierDerived`), nothing else.

## Configuration

Config is thin: it says *what to connect to*, not *what is trusted*. One
provider entry holds many models; each value is either the legacy `String`
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

The channel binding (TLS SPKI / provider E2EE key) and every claim are supplied
by the **verifier dynamically** — config carries no SPKI pin, no provenance
pins, and no asserted claims.

## Receipt linkage

The `upstream.verified` receipt event already carries `session_id`
(`add_upstream_verified_with_session`). Full trace:

```
request → receipt (x-receipt-id)
        → upstream.verified { session_id }
        → AttestedSession { claims (+ reasons), channel_binding, evidence }
```

## Storage: append-only signed JSONL

A tamper-evident log behind a session store trait (sibling to `ReceiptStore`).
Each line is a typed, gateway-signed record (`{ seq, ts, type, payload, sig }`).
On startup the log is replayed into an in-memory materialized index (by
`session_id`, and by `(provider, model_id)`). Properties:

- Append-only + per-record signature ⇒ integrity independent of at-rest
  sealing, and a natural feed for a future public transparency log.
- TEE sealing of the log file is a deployment concern; the per-record signature
  gives tamper-evidence regardless.
- Records are signed by `AciService` with the existing receipt key, keeping the
  store a pure appender.
- `InMemoryReceiptStore` stays for tests; the JSONL store is the durable impl.
- Compaction (snapshot + truncate) is future work; for the preview the log
  grows with a documented operator rotation step.

## API surface

All ACI verification artifacts live under `/v1/aci/` so they do not pollute the
OpenAI surface. Signed ACI artifacts keep `api_version: "aci/1"`; gateway-local
envelopes use `aci.<resource>.v1`.

Canonical (clean shapes):

- `GET /v1/aci/attestation/report?nonce=` — the bare gateway attestation report
  (preflight identity / liveness).
- `GET /v1/aci/receipts/{id}` — the signed ACI receipt (canonical value). `id`
  is the gateway `receipt_id` (preferred) or upstream `chat_id`. The
  `upstream.verified` event carries the typed claim verdicts inline (shallow
  audit) plus the content-addressed `session_id`.
- `GET /v1/aci/sessions/{session_id}` — the immutable session record
  (`aci.session.v1`), with full evidence + per-claim reasons (deep audit).
- `GET /v1/aci/sessions?provider=&model=` — a provider's attested sessions
  (`aci.session_list.v1`). This is the **preflight survey**: a read of the
  session store (see below), so a user can inspect the attested session + claims
  for a model *before* releasing any data.

### One store, one writer

The in-memory session store is the single source of truth, and the **background
upstream verification is its single writer**. The gateway already re-attests
every configured model-endpoint at startup and on a refresh cadence to stay
ready to serve; that same verification now seals each verified result into a
session (via an `UpstreamSessionSink` the service implements). So sessions are
established and kept fresh by the verification that runs anyway — there is no
separate import pass and no separate refresh loop, and therefore no drift.

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
   session store trait; an append-only signed JSONL impl with replay; keep
   in-memory for tests. Rework `record_attested_upstream_session` to seal the
   immutable session and persist it. Receipts already cite `session_id`.
2. **Typed claims from verifiers.** Each provider adapter populates
   `SessionClaims` (status + source + reason) from its verified evidence —
   including `serving_software_known_good` / `os_known_good` as the
   source-provenance surface.
3. **One store, one writer (preflight).** *Done.* The background upstream
   verification seals each verified result into the session store through an
   `UpstreamSessionSink`, so `/v1/aci/sessions` is a real preflight survey
   maintained by the verification that already runs for serving — no separate
   import or refresh loop, no drift. Still open: the per-model `endpoint` object
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
