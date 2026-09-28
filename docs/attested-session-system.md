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

Background upstream verification establishes and refreshes sessions before
traffic. Request completion records the session actually served on the
receipt's `upstream.verified` event. Both paths write through the same
process-owned store. `tests/service.rs::verified_upstream_binding_creates_attested_session`
covers the session created from a verified binding.

Two deadlines govern a stored session:

- The document's `expires_at` bounds its use for new forwarding decisions and
  for the list endpoint. The validity period reuses `receipt_ttl_seconds`.
- The store's `retention_until` bounds how long the session is served by id.
  Each citing receipt pushes it forward without touching the sealed bytes, so
  a session outlives every receipt that cites it (spec §8).

The audit trail runs:

```text
request -> receipt (X-Receipt-Id)
        -> upstream.verified { session_id }
        -> AttestedSession { claims with reasons, channel_binding, evidence }
```

## Preflight survey

`GET /v1/aci/sessions?upstream_name=&model=` reads the same store. A user can
inspect the verified identity, channel binding, and typed claims for a model,
and check its pinned SPKI, before releasing any data. The forwarding path
never trusts a stored session for freshness. It forwards only on a fresh
verification result.

## Storage: compacted JSONL

The durable store is an append-only log with one record per line:

```json
{"seq":0,"ts":1700000000,"type":"session","fingerprint":"<hex>","retention_until":1700003600,"payload_b64":"<sealed document>"}
```

On startup the gateway replays the log into an in-memory index. `payload_b64`
carries the sealed document bytes, and the session id is always recomputed from
them, never read from disk. `fingerprint` is a local key over a channel's
verified material that lets the request path find the current session for a
channel without sealing again. It is never served. Receipt signatures link
requests to session ids. At-rest durability and confidentiality remain
deployment concerns.

The gateway holds an advisory lock on a separate lock file so only one process
can own the log. On startup and hourly after that, it rewrites the live,
non-expired index through a synced temporary file and an atomic rename,
dropping duplicate, expired, malformed, or truncated history. The file names
and their `state_dir` location are listed in
[Runtime state files](configuration-reference.md#runtime-state-files).

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
- `gpu_verified` of `true`, set only for a verified and nonce-bound NVIDIA
  confidential-computing attestation, asserts `gpu_attested` as
  `verifier_derived`. It attests a genuine CC GPU, not its binding to the
  serving CPU TEE (spec §8.3). `false` is ambiguous and leaves the claim
  `unknown`.

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
- **Instance** (Chutes): one session per verified instance, so fleet changes do
  not change an unchanged instance's session id.

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
