# Attested Sessions: Implementation Notes

Attested sessions are immutable, content-addressed records of verified upstream
TEE channels. [spec/aci.md](../spec/aci.md) §8 specifies the record shape,
session ids, endpoints, and retention, and §8.3 specifies the typed claim
vocabulary. This page covers what the spec leaves to the implementation: how
this gateway seals and stores sessions, how it derives typed claims from
verifier results, and why the preflight survey exists.

The types live in `src/aggregator/session.rs` and the store in
`src/aggregator/session_store.rs`.

## Lifecycle in this gateway

Sealing a session is pure attestation. The verification fetches and checks the
provider's evidence, such as the TEE quote, the pinned TLS SPKI, and the
signing key, then serializes the verified material and typed claims into the
session document once. Those bytes are stored and always served
byte-identically, and the session id is their SHA-256 (spec §8). Sealing never
calls the model and never sees user data.

Background upstream verification refreshes the verifier cache. Listing and the
admission step seal sessions on demand through the same process-owned store.
Every verified forwarding attempt, with or without pins, acquires a retention
lease before calling the backend. The receipt cites the immutable session
admitted for the committed attempt; its `served_at` is that admission time.
Completion may occur after expiry. Response selection records the session
actually served on the receipt's
`upstream.verified` event. `tests/service.rs::verified_upstream_binding_creates_attested_session`
covers the session created from a verified binding.

Two deadlines govern a stored session:

- The document's `expires_at` is an upper bound for new forwarding decisions
  and listing, not a freshness guarantee. Replacement or expiry of the verifier
  cache can stop acceptance sooner. The validity period uses
  `attested_session_ttl_seconds` (default 3600), independently of receipt
  retention and verifier-cache TTL. Changes apply only to newly sealed documents.
- The store's `retention_until` bounds how long the session is served by id.
  Each citing receipt pushes it forward without touching the sealed bytes, so
  a session outlives every receipt that cites it (spec §8). Pending responses,
  retained failover responses, streams and middleware drafts hold leases that
  prevent eviction, including during compaction. Dropping the last owner releases
  its lease. A retained expired session remains fetchable but cannot authorize
  another admission.

Admission groups complete verified channels before filtering by pins, using the
same path as listing from current cached verification. One origin's TLS
alternatives share one session: bindings are sorted by JCS and exact duplicates
removed within that group. A pin authorizes the complete group, the backend
receives every accepted key, and the receipt cites that group whichever key
connects. Adding or removing a key changes the group's material.

Chutes and 0G retain independently selectable instance groups in verifier order;
0G uses that order as provider priority. They reject conflicting keys for an
instance, keep all admitted groups until the backend reports which instance
served, then cite that instance and release sibling leases. A missing or
unmatched serving instance identifier is an internal error. Mixed-origin and
other undefined multi-binding shapes fail closed.

If an adapter withholds an unverified error body without identifying the
serving instance, the gateway preserves its HTTP status and cites no instance
session. Buffered receipts record failed verification and the forwarded
request; streaming errors keep their existing receipt behavior.

The audit trail runs:

```text
request -> receipt (X-Receipt-Id)
        -> upstream.verified { session_id }
        -> AttestedSession { claims with reasons, channel_binding, evidence }
```

## Reviewed material and snapshot evidence

Evidence-independent reuse is an adapter capability. Complete `near-ai/1`,
`phala-direct/1`, `chutes/1` (per instance), `privatemode/1` and
`aci-service/1`
claims require the matching provider type and built-in verifier ID. Missing,
unknown, mismatched or incomplete material remains evidence-sensitive. ArmetAI,
SecretAI (both TDX and SNP), Tinfoil (TDX and SNP), 0G, generic external verifiers
and legacy Chutes outputs
remain evidence-sensitive; neither provider names nor markers alone opt in.
Shape validation does not authenticate facts; the configured verifier owns that.

The private fingerprint for reviewed material is SHA-256 of JCS over
`schema: "aci-session-material/1"`, provider type, upstream name, endpoint,
verifier ID, identity, channel bindings and all recorded claims. TDX profiles
preserve every supported TD10/TD15 security field except report data; non-freshness
report-data inputs are retained as identity/bindings/claims. Rust checks the
report format, hex field lengths and recomputed report digest.
Privatemode retains the observed manifest digest and `manifest_observed_at`;
this observation is not bound to the active inference secret.

Unreviewed material fingerprints the evidence digest by default. Identical complete
0G broker evidence can therefore reuse a session after fresh verification; changed
evidence selects another fingerprint. The verifier explicitly declares when its
evidence does not bind platform state: currently Tinfoil's SDK summary omits the
authenticated quote. Such events carry a gateway-local round token, created on each
verifier run and cloned on cache hits. The token separates fingerprints across
independent rounds without inserting a random value into provider evidence. It
does not detect or expose platform changes hidden by the SDK summary. Reviewed
profiles ignore this token. Byte-identical Tinfoil documents sealed in the same
Unix second may still have the same content-addressed ID.

ACI-service identity uses the accepted `ReportBinding` digest of the original
keyset JSON, including unknown members, plus explicit `subject` (string or null)
and `keyset_not_after`. Its `aci-service/1` claims retain the authenticated TDX
projection, DCAP TCB/advisories, measured compose/app ID, published source
provenance and normalized receipt-custody identities (without signatures).
Provenance is published metadata, not independently rebuilt source. Receipt-key
derivation still relies on measured workload code; other key roles are not
thereby authenticated. Failed, offline or unevaluable appraisal cannot emit the
stable profile; parsed diagnostics and human-readable check details never supply
it. Session expiry is capped at the accepted keyset's `not_after`, independently
of the verifier cache cap, and admission at that deadline is rejected.

Equal reviewed material reuses an unexpired document byte for byte, including
its establishing evidence, without extending validity. Binding, identity,
measurement, security outcome or claim changes select a different fingerprint.
A → B → A can reuse unexpired A. Failure alone does not revoke history or force
a generation; forwarding still needs usable current verification. Recovery may
reuse equal material. Session expiry may reseal from a still-usable verifier cache
without network verification: creation time does not mean evidence collection time.

A session contains an establishing-evidence snapshot. A reused session does not
supply evidence from every later verification round. Session and evidence hashes
prove integrity; they do not perform provider-specific deep audit (§9.2(4)). Later
revalidation is the attested aggregator's assertion. A policy requiring independent
evidence for every authorizing round cannot rely on a snapshot alone.

Phala and Chutes GPU checks are supplemental outcomes. Their current NVIDIA paths
decode JWTs without signature verification and do not bind device identity to the
serving CPU. Snapshot reuse establishes neither authenticated GPU identity nor
continued use of a particular GPU. ArmetAI and SecretAI require separately reviewed,
complete CPU/GPU projections before they can reuse across evidence changes.

## Migration: pre-#456 to this version

New reviewed profiles (`near-ai/1`, `phala-direct/1`, `chutes/1` per instance,
`privatemode/1`, `aci-service/1`) select one replacement session for their new
fingerprint domain and complete claims. ACI-service identity includes the accepted
original keyset digest, subject and expiry. Multi-key TLS channels replace former
singleton sessions with a session covering the complete group; old singleton IDs
cannot authorize a subset of that group.

0G previously omitted the evidence digest from its fingerprint. It selects one
replacement session when the digest is first included, then reuses it while the
complete evidence remains identical. Tinfoil's incomplete SDK summary uses a
fresh gateway-local round token on every verifier run, selecting a new session
for each fresh round even when summaries match; cache hits retain the token and
session.
Other unreviewed material keeps its evidence-digest fingerprint.

JSONL fingerprints remain opaque. Existing records, IDs and pins are never
rewritten or backfilled; old and new records coexist under normal retention.
Stored session and receipt wire documents remain compatible. Fixed old pins
receive `412 session_not_accepted`: fetch, audit and explicitly accept the
replacement. Existing policy-based refresh may do so only under its existing
policy; there is no unpinned downgrade or ID alias. Restart reuse requires current
verification of the complete group. Rollback can select a retained old fingerprint
or seal a legacy session and requires the same explicit audit and repinning.

Rust integrations initialize the optional `UpstreamVerifiedEvent::verification_round`
field to `None` for complete evidence; an adapter whose evidence omits platform
state creates a `VerificationRound::fresh()` token on fresh verification and
clones it on cache hits. Callers of the removed KMS root-only helper use
`verify_dstack_kms_receipt_custody(...)?.root_public_key`.

### Custom session stores

`SessionStore::current_or_seal` returns an opaque `SessionLease` instead of a
session ID, and `extend_retention` extends that lease's indexed entry before
receipt publication. Lease IDs and validity timestamps have read-only accessors;
leases cannot be constructed externally because their ownership token must match
the retained entry.

External wrappers must delegate both operations to the same built-in
`InMemorySessionStore` or `JsonlSessionStore`, keeping the returned lease intact.
Implementing a separate storage engine is a source-breaking migration that needs
a supported lease-construction/ownership API; this version does not expose one.
Do not synthesize leases or mix acquisition and retention across stores.

## Preflight survey

`GET /v1/aci/sessions?upstream_name=&model=` derives sessions from the verifier's
current cache, the same state the pin gate consults. For the queried channels,
the list is exactly the sessions the gate accepts now without re-verifying. It never
performs fresh verification. An empty list means no verified state is available
now; absence does not imply refusal, because a stable-evidence verifier can
re-derive the same id after fresh verification. Chutes lists only instances in
the current cached event.

Pins apply to the routed channel. Use `?model=<public-alias>` to survey that
model's channels, using the same alias mapping as requests. Without middleware,
only the default first route is listed; with middleware, all configured routes
serving the model are listed and pin-mismatched candidates are skipped. A broad
list can contain sessions that a particular request will not route to.

Failed refresh keeps the previous entry until it expires; an older in-flight
result cannot replace the listing. Concurrent sealing of identical material
yields one id.

## Storage: compacted JSONL

The durable store is an append-only log with one record per line:

```json
{"seq":0,"ts":1700000000,"type":"evidence","payload_b64":"<bundle bytes>"}
{"seq":1,"ts":1700000000,"type":"attested_session","fingerprint":"<hex>","retention_until":1700003600,"content_type":"application/json","payload_b64":"<document without evidence.data>"}
```

The log has two kinds of records. An `evidence` record holds one evidence
bundle. An `attested_session` record holds a session document with its
`evidence.data` removed; the document still has `evidence.digest`, the hash of
its bundle. Keeping bundles separate means a bundle is stored only once, even
when many sessions use it. This matters for Chutes, where every instance
session of a chute uses the same bundle. Every stored session has its bundle.

On startup the gateway reads the log into memory. For each session it finds
the bundle whose hash matches `evidence.digest`, puts the bundle back into the
document, and computes the session ID from the result. The ID is never read
from disk. If a bundle was altered on disk, its hash no longer matches, so the
sessions that use it are dropped. Records of any other type are ignored, so
the gateway can start on a log written by an older release.

`fingerprint` is a local key over a channel's verified material. The gateway
uses it to find a channel's current session without building the document
again. It is never served. Receipt signatures link requests to session IDs.
Backing up the log and keeping it private on disk are up to the deployment.

The gateway locks a separate lock file so only one process can use the log.
On startup and every hour after that, it writes retained sessions, including
expired sessions with outstanding leases, to a new file, flushes it to disk,
and swaps it in with a single rename. This drops duplicate, unheld lapsed, or
damaged records and any bundle that
no remaining session uses. The file names and their `state_dir` location are
listed in [Runtime state files](configuration-reference.md#runtime-state-files).

Retention extensions update memory without appending records and are persisted
by compaction. An uncompacted extension can be lost on restart; receipts and
leases are process-local and are lost too. Replay preserves document IDs and
expiry, but never warms the verifier cache. Existing pins can survive restart
once the same current material is verified and their documents remain valid.

## How claims are derived

A session carries six typed claims: `tee_attested`, `tcb_up_to_date`,
`os_known_good`, `serving_software_known_good`, `gpu_attested`, and
`model_weights_provenance`. Each claim has a status (`asserted`, `refuted`, or
`unknown`) and, when not `unknown`, a `source` and a `reason`. Current mappers
use two sources: `hardware_proven`, read from the verified quote or its
collateral, and `verifier_derived`, computed by the verifier from verified
evidence.

`session_claims_for_event` in `src/aggregator/service/claims.rs` builds the
claims from a verified upstream event:

- A `failed` result asserts nothing.
- The event's stable `provider_type`, distinct from the operator's per-entry
  `name`, selects one claim mapper. NEAR AI, Chutes, and Phala direct share the
  Intel TDX mapper. Tinfoil and SecretAI have their own mappers. Privatemode's
  mapper asserts only `tee_attested` as `verifier_derived`, because its
  manifest observation is not bound to the request's secret. Every other
  verifier, including `aci-service`, uses the generic mapper, which asserts
  only `tee_attested` as `verifier_derived`.
- A mapper asserts a claim only when its verifier's evidence backs it. Missing
  evidence leaves the claim `unknown`; it is never asserted by policy.
- The raw `provider_claims` are copied verbatim into `claims.extra`. Their key
  names are a stable contract (spec §8.3).

Mappers that read these provider-reported facts read them the same way:

- `tcb_status` of `UpToDate` asserts `tcb_up_to_date` as `hardware_proven`.
  Any other reported status refutes it: the quote proves a stale TCB, which the
  gateway records instead of rejecting. No status leaves it `unknown`.
- `production_os_image` of `true` asserts `os_known_good` as
  `verifier_derived`, and `false` refutes it.
- `gpu_verified` of `true` asserts `gpu_attested` as `verifier_derived`.
  For Phala and Chutes it records the supplemental check's accepted,
  nonce-matched outcome, with the unsigned-JWT and CPU-binding limits above;
  it must not be read as an authenticated device identity or continuity claim.
  ArmetAI and SecretAI use mandatory signed GPU checks with their own CPU
  bindings. `false` is ambiguous and leaves the claim `unknown`.

`model_weights_provenance` is `unknown` for every current verifier. Each
provider's verification page states which claims its adapter asserts and why:
[ACI service](providers/aci-service/verification.md),
[Chutes](providers/chutes/verification.md),
[NEAR AI](providers/near-ai/verification.md),
[Phala direct](providers/phala-direct/verification.md),
[Privatemode](providers/privatemode/verification.md),
[SecretAI](providers/secret-ai/verification.md), and
[Tinfoil](providers/tinfoil/verification.md).

## Session scope

A provider's attestation scope decides what one session covers:

- **Router** (NEAR AI, Tinfoil, SecretAI, Privatemode): one session per router
  or proxy channel. The served model is recorded on the receipt, not in the
  session.
- **Model** (Phala direct, `aci-service`): one session per upstream model.
- **Instance** (Chutes and 0G): one session per verified instance. With reviewed
  Chutes material, unrelated fleet changes preserve an unchanged instance's
  session ID. 0G remains evidence-sensitive: refreshed evidence, including changes
  to fleet evidence, can replace its sessions.

A bridged provider verifier declares its scope in `attested_scope`. The
gateway rejects a verified result whose declared scope differs from the
provider's scope, and a router provider result that declares none.

## Source-code provenance

The verifier, not a gateway schema, owns source-code provenance: whether a
measured image or compose maps to reviewed source. The verifier chooses how to
establish it, for example by matching known measurements, a pinned image
digest, a signed SLSA or in-toto attestation, or a reproducible build. It
reports the result as the `serving_software_known_good` and `os_known_good`
claims, with a reason such as `"compose hash matches reviewed image X"`. The
gateway records and serves these claims verbatim, so a stronger provenance
method is a change inside a verifier, not a change to the session model or
configuration.

## Configuration

Optional provider policy, such as `accepted_subjects`,
`accepted_image_digests`, and `accepted_dstack_kms_root_public_keys`, narrows
the accepted identities. It never supplies claims. The channel binding and
every claim come from the verifier at verification time, so configuration
never carries a raw SPKI pin or an asserted claim. The fields are listed in
[Upstream fields](configuration-reference.md#upstream-fields).

## References

- [spec/aci.md](../spec/aci.md) §8 defines the session record, ids,
  endpoints, and retention; §8.3 the claim vocabulary; and §9.2 session
  verification.
- [Provider audit criteria](providers/audit-criteria.md) are the criteria
  behind the claim model.
- [Upstream verification lifecycle](upstream-verification-lifecycle.md)
  explains verification caching and refresh, which are separate from session
  records.
