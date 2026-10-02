# Known Gaps Between This Implementation and the ACI Spec

This page lists the places where the gateway and its clients fall short of
[the ACI spec](../../spec/aci.md) or behave differently from it. The spec is
the reference. Each item is a known compromise in this implementation, not a
change to the spec, and a candidate for future work.

## Verifier coverage

1. **Key custody is checked only for the receipt key, and only on request.**
   The spec asks verifiers to check where the workload's private keys live
   (§9.1 check 5). `pap` checks this for the receipt key when you pass
   `--accept-subject` and `--accept-dstack-kms-root-public-key`: the dstack
   KMS signature chain must lead from the measured app ID to a KMS root you
   accept, and must name a receipt key from the attested keyset. Without those
   flags `pap` skips the check, and the TypeScript verifier always skips it.
   Neither client checks custody of the E2EE or TLS keys (see items 13 and 14).

   A skipped check does not change the exit code, so a run can end with
   `VERIFIED` and exit 0 without checking custody. The transcript always shows
   the skip and the reason. If your policy requires custody, look at the
   status of check id-5, not only the exit code.

2. **The compose file is measured, but the source is not rebuilt.** When the
   service publishes its compose file, check id-4 confirms it is the file
   measured into the hardware quote, and `--accept-compose` limits which
   compose files you accept. Nothing rebuilds the code from the repository and
   commit the report names, so those two fields are only labels, and deciding
   whether to trust them is up to you. Against a live service, both verifiers
   fail check id-4 when the service publishes no compose file. Only an offline
   audit of a saved report skips the check instead.

## Gateway behavior

3. **Receipts are kept only in memory.** A receipt is kept for
   `receipt_ttl_seconds` and is lost if the gateway restarts. The spec lets an
   implementation choose its retention period (§7.1), but a restart shortens
   it without warning. Attested sessions are stored on disk, survive restarts,
   and are kept as long as any receipt cites them.

4. **A Chutes session keeps the evidence from the round that created it.**
   Chutes returns one evidence bundle for all of a chute's instances, and the
   bundle changes on every verification round because each round uses a new
   nonce. If the bundle were part of what makes a session unique, every round
   would create a new session for every instance. An earlier change did
   exactly that (#142) and was reverted (#145): the session log grew without
   bound, and replaying it at startup could run out of memory before the
   gateway served a single request.

   Instead, a Chutes instance session stores the bundle from the round that
   created it, and the bundle is left out of the fingerprint the gateway uses
   to find a channel's current session. Re-verifying the same instance while
   the session is valid reuses that session. A real change, such as a
   different TCB status, GPU result, or key, still creates a new session.
   Nothing enforces this automatically, so it stays a design rule: the
   fingerprint must not include anything that changes on every round.

   The log stores each bundle once, and every instance session that uses it
   refers to it by digest. Every stored session has its evidence: the gateway
   rejects a verified result without evidence, and the store refuses a
   session without it.

   Two limits remain. The stored evidence proves the instance's state when the
   session was created, not at the time of a later request; each request is
   still sent only over the instance's attested key. And no client can yet
   check what the Chutes evidence itself proves (§9.2 check 4, see item 17).
   Clients can check that the evidence matches its digest (§9.2 check 2).

5. **Failed streaming requests get no receipt.** When the upstream rejects a
   streaming request, the gateway returns the error without a receipt, as
   dstack-vllm-proxy did. The same error on a non-streaming request does get a
   receipt. No inference happened, so the spec may not require a receipt, but
   the two paths behave differently.

6. **Session pinning is not exact for Chutes.** When a request lists the
   sessions it accepts (§5.3), the gateway checks the list before forwarding,
   against the channel's current sessions. A Chutes route serves many
   instances, and the gateway learns which instance answered only after the
   response. So an unlisted instance can serve a request that listed one of
   its siblings. The receipt names the session that actually served it, so the
   client's receipt check (§9.3 check 6) catches this.

7. **E2EE replay protection works within one process.** The
   [E2EE v2 protocol](../../spec/e2ee-v2.md#7-key-selection-validation-and-replay-protection)
   requires the gateway to reject a request that reuses a client key, service
   key and nonce it has already accepted. The gateway remembers accepted
   requests only in its own process. Replicas that share one keyset could each
   accept the same captured request once, unless the deployment sends a client
   to one replica or shares the replay state.

8. **The session list shows sessions as current after they are replaced.** A
   session's `expires_at` is set one receipt TTL ahead (an hour by default).
   But for most providers each verification round, every
   `verifier_cache_seconds` (five minutes by default), brings new evidence and
   so a new session. The list keeps showing the replaced session until its
   `expires_at`, and a client that pins it is refused with
   `session_not_accepted` even though the gateway still lists it. The fix is
   to end a session's validity when a newer one replaces it.

9. **A channel with several keys becomes several sessions.** The gateway
   creates one session per channel binding. For Chutes that is right, because
   each binding is a separate instance. But an `aci-service` upstream that
   publishes several TLS keys for one origin gets one session per key. Each
   session claims a narrower binding than the gateway enforces, since the
   connection accepts any of the keys. The receipt cites the first session, so
   a client that pinned a sibling session passes the gateway's check and then
   fails its own receipt check. The fix is to group the bindings of one
   channel into one session.

10. **A session can expire before the receipts that cite it.** A session's
    retention is set when the session is created at the start of a request,
    while the receipt's retention is set when the response ends. For a long
    stream, the session can be dropped while its receipt is still served. The
    spec requires a session to outlive every receipt that cites it (§8). The
    gap is under a second for normal requests and as long as the stream for
    streaming ones.

11. **A verified request can be recorded as failed.** The gateway matches the
    instance that served a request against its instance sessions. If an
    external verifier reports an E2EE binding without a `key_id` (the field is
    optional) and the backend then reports which instance served, nothing
    matches, and the receipt records the request as failed even though it was
    verified and served. The bundled verifiers always set `key_id`, so this
    cannot happen today.

12. **The Chutes backend adds a field after the request is hashed.** The
    receipt's `request.forwarded` hash covers the body the gateway prepared.
    The Chutes backend then adds `e2e_response_pk` to that body before
    encrypting and sending it, so the body Chutes reads has one field the
    receipt does not cover. ACI treats encryption to the upstream as part of
    the channel, but this is a field in the body, not part of the encryption.

13. **Nothing proves where the TLS private key lives.** The keyset publishes
    the public key of a certificate mounted into the gateway, and no custody
    evidence covers it. A client that pins that key cannot tell from the
    report whether TLS ends inside the TEE; that depends on how the deployment
    terminates TLS. The [E2EE v2 extension](../../spec/e2ee-v2.md) protects
    request content without relying on this. The fix is to publish custody
    evidence for the TLS key.

14. **The custody policy covers only the receipt key.** The dstack custody
    check matches the receipt key against the attested keyset, but never
    checks the E2EE custody entries against the published E2EE keys, and
    nothing covers the TLS key. The spec asks a custody policy to cover the
    receipt, E2EE and TLS keys (§3.3).

15. **Nothing limits how far ahead a keyset can expire.** The spec says a
    verifier should reject an expiry that is implausibly far away (§3.1). No
    verifier here does, and the gateway accepts any configured lifetime.
    Expiry is the only way to revoke keys without coordination (§3.4), so a
    keyset valid for decades cannot be revoked that way.

16. **The `aci-service` upstream verifier trusts `image_digest` without
    proof.** It accepts an upstream if either its app ID or the image digest
    in its report is on the allowlist. The app ID is measured: it comes from
    the verified event log, and the compose file is checked against the
    measured compose hash. The image digest is only the upstream's own claim,
    and nothing ties it to a measurement, so it admits an upstream on its word
    alone. The spec says to trust provenance only when measured evidence
    backs it (§4.1). The repository and commit fields are not checked either
    way.

    The KMS custody chain has a related limit. It proves which KMS key belongs
    to the app, but the step from that key to the published receipt key
    happens in the workload's code, so it is only as trustworthy as the
    measured code. That is another reason the policy must rely on measured
    values. The fix is to tie the image digest to the verified compose file,
    or stop accepting it on its own.

17. **Session audits check the evidence's integrity, not what it proves.**
    Both verifiers check that a cited session hashes to its ID, that it was
    valid when the receipt was issued, and that its evidence matches its
    digest (§9.2 checks 1 and 2). `pap` can also require specific claims with
    `--require-claim` (§9.2 check 3); the TypeScript verifier only displays
    them. Neither verifier re-checks what the evidence proves (§9.2 check 4).
    Possible next steps are a required-claims option for the TypeScript
    verifier and a way to reuse the provider verifiers to re-check evidence
    on the client.

## Tooling

18. **TypeScript CI does not run on spec-only changes.** The TypeScript
    verifier's tests compare against the spec's test vectors byte for byte,
    and its CI runs for changes under `clients/`. An edit to
    `spec/test-vectors.md` alone does not trigger it. The Rust test
    `tests/spec_vectors.rs` still catches the mismatch.

## Intentional differences

19. **Compatibility with dstack-vllm-proxy.** `/v1/attestation/report`,
    `/v1/signature/{id}`, and the `X-Signing-Algo` E2EE mode serve clients
    written before ACI and use their own report format. The k256 key they
    share with the E2EE v2 secp256k1 suite appears in the ACI keyset with its
    custody evidence; the legacy Ed25519 key stays outside ACI. As the spec
    requires (Appendix B), these routes never change ACI reports, receipts, or
    sessions.
