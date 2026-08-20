# Reference Implementation vs. ACI Spec: Known Gaps

Where this implementation currently falls short of, or diverges from,
[the ACI Spec](../../spec/aci.md). The spec is authoritative; these are
implementation compromises, not spec changes. Each item is a candidate work
item.

## Verifier coverage

1. **The `aci` CLI has no custody policy.** §9.1(5) requires the verifier
   policy to check private-key custody (for this deployment, the dstack KMS
   signature chain in `attestation.evidence.key_custody`). The in-tree
   dstack chain validation (`src/aci/verifier/dstack.rs`) is not wired into
   the CLI, so `aci verify` reports id-5 as an honest `skip`, never a pass.
   The top-line verdict and exit code do not distinguish a skip from a pass:
   a run can end `VERIFIED` (exit 0) with custody unevaluated. The skip and
   its reason are always printed in the transcript and counted in the
   verdict line; a relying party that requires §9.1(5) must gate on the
   id-5 status, not on the exit code alone.

2. **Provenance is measured, not rebuilt.** When the service publishes
   `app_compose`, id-4 verifies `sha256(app_compose)` equals the `compose-hash`
   measured into the quote's RTMR3, and `--accept-compose` pins that value to
   an operator allowlist (§1.3). Nothing rebuilds `repo_url`/`repo_commit`
   from source or ties them to the measurement, so those fields stay a label
   to read rather than evidence; appraising them is the operator's job.
   Against a live service both verifiers fail id-4 when no `app_compose`
   backs the claim (§9.1(4)); only an offline audit of a stored report
   records the honest skip.

## Service conformance

3. **Receipts are in-memory only.** Receipt retention is bounded by
   `receipt_ttl_seconds` and lost on restart. The spec permits a bounded,
   implementation-defined retention period (§7.1), but a restart shortens it
   silently. Sessions do better: the JSONL store survives restarts and
   extends retention per citing receipt (§8 retention rule).

4. **Chutes per-instance sessions carry no §8.2 evidence.** The Chutes
   verifier's raw evidence is fleet-wide and nonce-bound, so sealing it into
   each per-instance session would mint a new session id for every
   verification round and every fleet change. The implementation instead
   seals per-instance sessions with an empty `evidence` object
   (`record_attested_upstream_session`), keeping them content-addressed on
   per-instance facts only. Consequence: the §9.2 deep-audit step fails
   closed on Chutes-cited sessions (`aci` CLI upstream-2; verifier-ts
   `checkSessionEvidence`) — a §9.2(4) deep audit is impossible for Chutes
   even once built (see the §9.2 audits item below), while receipt
   verification and the §9.1/§9.3 checks are unaffected. The
   session store still accepts these records (its §8.2 check rejects only
   evidence whose `data` does not hash to `digest`).

   Sealing the evidence in was tried and reverted. The predicted new session
   id per round is what happens, and it compounds: each round appends a fresh
   record per instance instead of resolving to the existing one, and each
   record now carries the fleet-wide bundle, so the log grows without bound
   relative to the live set. Startup replays that log into the index, so a
   long enough gap since the last compaction exhausts memory before the
   process serves a request — and because the kill lands during startup, it
   repeats on every restart with no path back except moving the file aside.

   So the fix has to keep the evidence *out* of what the session id commits
   to. Emitting a per-instance evidence slice, the work item below, removes
   the fleet-wide half but not the nonce-bound half, so on its own it still
   mints a new id per round. Retaining the evidence under its own digest and
   linking sessions to it out of band addresses both, and retains every
   round's evidence rather than only the latest. Either way, two properties
   the store depends on need stating explicitly, because nothing currently
   enforces them: a session's identity must not commit to anything that
   changes per verification round, and replay must stay proportional to the
   live set rather than to everything appended since the last compaction.

5. **Streaming upstream errors carry no receipt.** A streaming request whose
   upstream answers non-200 is returned as a buffered error without a
   receipt (inherited dstack-vllm-proxy behavior,
   `forward_chat_completion_stream_request`), while the buffered path issues
   a receipt for the same upstream error status. Arguably outside §1.4(5) —
   no inference completed — but the coverage is asymmetric.

6. **§5.3 membership is best-effort for multi-instance backends.** The
   pinned-session gate runs before forwarding against the channel's current
   session ids. A Chutes-style backend fronts many instances behind one
   route, and the serving instance is known only after the response, so a
   non-listed instance can serve when a sibling instance was listed. The
   receipt's cited id exposes this to the client's §9.3(6) check.

7. **The E2EE v2 replay cache is per process.** The
   [v2 protocol](../../spec/e2ee-v2.md#7-key-selection-validation-and-replay-protection)
   requires rejection of a repeated
   `(client_public_key, service_public_key, nonce)` tuple inside the acceptance
   window. `claim_e2ee_replay` keeps that state in one
   process. Replicas that share the same workload keyset can each accept the
   same captured request once unless the deployment provides affinity or a
   shared replay store.

8. **Session validity is advertised far longer than a session can actually
    serve.** `expires_at` is set to `now + receipt_ttl_seconds` (default
    3600), but each verification round mints a fresh nonce, so the evidence
    digest — part of the channel fingerprint — changes every
    `verifier_cache_seconds` (default 300) and a new session supersedes the
    old one. The list endpoint keeps advertising the superseded session as
    current until its `expires_at`, so a client that verified and pinned it
    (§5.3) is refused `session_not_accepted` while the service still lists
    it. Fix direction: end a session's validity period when a re-verification
    supersedes it, so "current" means current.

9. **A channel with several bindings is split into one session per
    binding.** `record_attested_upstream_session` seals a session per entry
    in `channel_bindings`. For a Chutes-style backend that is correct (one
    session per instance), but an `aci-service` upstream publishing several
    service-wide TLS pins for one origin yields several sessions whose
    records each state a tighter binding than the aggregator enforced (the
    TLS client accepts any of the pins). The receipt cites the first, so a
    client that pinned a sibling session passes the membership gate and then
    fails its own §9.3(6) check. Fix direction: group bindings of one channel
    into one session, keyed on what makes a channel distinct.

10. **Session retention can lapse before the receipts citing it.** Session
    `retention_until` is fixed at seal time — stream start — while the
    receipt's expiry is computed at stream end, so for a long stream the
    session can be evicted while its citing receipt is still served. §8
    requires the session to outlast every receipt citing it. Sub-second
    window on the buffered paths, stream-duration window on the streaming
    ones.

11. **A verified, served request can be recorded as `result: "failed"`.**
    `cite_served_session` matches a reported served instance only against
    sealed sessions carrying an `instance_key`. An external verifier that
    returns an `e2ee_public_key_sha256` binding without `key_id` (optional in
    the contract) while the backend reports a served instance matches
    nothing, and the receipt records the failed form on a request that was
    verified and served. Unreachable with the bundled bridges, which always
    set `key_id`.

12. **The Chutes backend adds a member to the forwarded body after
    hashing.** `request.forwarded` hashes `prepared.request.body`, and
    `build_chutes_e2ee_request` then inserts `e2e_response_pk` into that JSON
    before encrypting and sending. The JSON the upstream parses therefore
    carries one member the signed hash does not commit to. ACI treats upstream
    encryption as a channel-binding detail outside client-facing E2EE
    extensions, but this is a body member, not encryption framing.

13. **TLS keys are attested without custody evidence.** The keyset publishes
    the SPKI of a mounted certificate whose private key lives in the external
    TLS terminator, not the workload, and no `key_custody` entry covers the
    TLS role — so no verifier policy can check §3.3 custody for it, and a
    client pinning that SPKI cannot tell whether TLS terminates inside the
    TEE. The [E2EE v2 extension](../../spec/e2ee-v2.md) is the currently
    supported mechanism that does not depend on this. Fix direction: terminate
    TLS in the workload, or publish custody evidence for the terminator.

14. **The dstack custody policy checks only the receipt key.**
    `verify_dstack_kms_receipt_custody` matches the `receipt` role against
    `receipt_signing_keys`; the `e2ee-secp256k1` and `e2ee-x25519` custody
    entries are never matched against `e2ee_public_keys`, and TLS is not
    covered. §3.3 requires a policy to specify custody for the receipt, E2EE
    and TLS keys.

15. **No plausibility bound on `not_after`.** §3.1 says a verifier SHOULD
    reject an implausibly distant expiry; no verifier here does, and the
    service accepts any configured lifetime. §3.4's automatic expiry is the
    only revocation that needs no coordination, so an absurd `not_after`
    nullifies it.

16. **The `aci-service` upstream verifier trusts `image_digest`
    uncorroborated.** Its policy accepts an upstream when the attested
    app id is allowlisted **or** the report's `source_provenance.image_digest`
    is (`AciServiceVerifierPolicy::accepts_measured`). The first path is
    measured: acceptance keys on `app-id:0x<hex>` of the app id the verified
    RTMR3 event log yields, and the compose preimage is checked against the
    measured `compose-hash` (an upstream publishing no `app_compose` fails
    closed). A keyset `subject` is the workload's own claim, so it may only
    restate that measured value. But `image_digest` is a self-asserted report
    field that nothing ties to that measurement, so the second path admits an
    upstream on a claim alone — §4.1 says a verifier trusts provenance "only
    when corroborated by measured evidence, like the §4.2 compose-hash path".
    `repo_url` / `repo_commit` are corroborated by neither path. Related: the
    KMS receipt-custody chain is pinned to the `aci.receipt.ed25519.v1`
    purpose, but the link from the KMS-derived k256 scalar to the published
    Ed25519 receipt key rests on the measured workload code — another reason
    the anchor must stay measured. Fix direction: anchor `image_digest` to the
    verified compose, or drop it as a policy anchor.

17. **The §9.2 session audits stop short of evidence appraisal.** Both
    verifiers prove the cited session hashes to its id, the validity window
    holds, and the evidence data hashes to its digest — §9.2(1)-(2). The
    `aci` CLI also appraises the typed claims against a caller policy
    (§9.2(3), `--require-claim` on audit/sessions/send/serve); verifier-ts
    still only formats claims into the upstream detail. Neither implements
    §9.2(4), appraising the evidence itself. Candidate work items: a
    `requiredClaims` input for verifier-ts, and an evidence-appraisal hook
    reusing the provider-verifier logic.

## Stale surroundings

18. **The live E2E scripts predate the simplified protocol.** Parts of
   `scripts/live_e2e/` (e.g. `cases/embeddings.py`, `cases/lifecycle.py`)
   still assert removed transparency events. The multi-upstream smoke scripts
   and attested-session case have been migrated to the simplified schema, and
   the in-process integration suites (`tests/`) cover the new protocol. The
   remaining live lifecycle cases need the same pass.

19. **Client CI triggers are path-scoped.** `verifier-ts` tests pin the spec
   test vectors byte-for-byte, but the workflow triggers only on
   `clients/verifier-ts/**`, so an edit to `spec/test-vectors.md` alone does
   not rerun them (Rust CI catches drift via `tests/spec_vectors.rs`).

## Beyond-spec surfaces (intentional, keep honest)

20. **Legacy dstack-vllm-proxy compatibility** (the Appendix B non-ACI-surfaces rule).
    `/v1/attestation/report` (separate report-data layout, injected
    `signing_address` / `intel_quote` / `nvidia_payload`), `/v1/signature/{id}`,
    and the `X-Signing-Algo` E2EE mode serve pre-ACI clients. The shared k256
    key also serves the E2EE v2 secp256k1 suite, so its KMS custody evidence is
    a keyset role in `/v1/aci/attestation`; the legacy Ed25519 key stays
    outside ACI artifacts. The spec's rule that compatibility
    surfaces must not alter ACI artifacts holds: report, receipt, and session
    bytes are identical with or without compatibility parameters.
