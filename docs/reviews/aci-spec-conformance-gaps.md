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
   Neither client checks custody of the E2EE or TLS keys (see items 8 and 9).

   A skipped check does not change the exit code, so a run can end with
   `VERIFIED` and exit 0 without checking custody. The transcript always shows
   the skip and the reason. If your policy requires custody, look at the
   status of check id-5, not only the exit code.

2. **Verifiers do not read the source commit from the compose file.** When
   the service publishes its compose file, check id-4 confirms it is the file
   measured into the hardware quote, and `--accept-compose` limits which
   compose files you accept. With the [git-launcher deployment](../../deploy/README.md),
   the compose file names the gateway repository and an exact commit, and the
   confidential VM checks out and builds that commit itself, so the measured
   compose file identifies the source. The verifiers do not take the commit
   from the compose file, though. The report's `repo_url` and `repo_commit`
   are the gateway's own statement, and no verifier checks them against the
   compose file. To confirm the source, read the commit from the compose file
   as [Verify the deployment](../../deploy/README.md#verify-the-deployment)
   describes. Against a live service, both verifiers fail check id-4 when the
   service publishes no compose file. Only an offline audit of a saved report
   skips the check instead.

## Gateway behavior

3. **Receipts are kept only in memory.** A receipt is kept for
   `receipt_ttl_seconds` and is lost if the gateway restarts. The spec lets an
   implementation choose its retention period (§7.1), but a restart shortens
   it without warning. Attested sessions are stored on disk, survive restarts,
   and are kept as long as any receipt cites them.

4. **Failed streaming requests get no receipt.** When the upstream rejects a
   streaming request, the gateway returns the error without a receipt, as
   dstack-vllm-proxy did. The same error on a non-streaming request does get a
   receipt. No inference happened, so the spec may not require a receipt, but
   the two paths behave differently.

5. **E2EE replay protection works within one process.** The
   [E2EE v2 protocol](../../spec/e2ee-v2.md#7-key-selection-validation-and-replay-protection)
   requires the gateway to reject a request that reuses a client key, service
   key and nonce it has already accepted. The gateway remembers accepted
   requests only in its own process. Replicas that share one keyset could each
   accept the same captured request once, unless the deployment sends a client
   to one replica or shares the replay state.

6. **Session pins are local to each replica.** Each gateway process keeps
   its own session store and verifier cache. A replica accepts a pin only
   when its current verified material selects that session from its store.
   Behind a load balancer, a client that lists sessions on one replica and
   sends a pinned request to another can be refused with
   `session_not_accepted`. Stable material does not synchronize independently
   established session IDs, and sharing stored sessions alone does not
   replace current verification. The deployment must keep pinned requests on
   the replica that listed the sessions or coordinate both the session store
   and current verified material across replicas.

7. **The Chutes backend adds a field after the request is hashed.** The
   receipt's `request.forwarded` hash covers the body the gateway prepared.
   The Chutes backend then adds `e2e_response_pk` to that body before
   encrypting and sending it, so the body Chutes reads has one field the
   receipt does not cover. ACI treats encryption to the upstream as part of
   the channel, but this is a field in the body, not part of the encryption.

8. **Nothing proves where the TLS private key lives.** The keyset publishes
   the public key of a certificate mounted into the gateway, and no custody
   evidence covers it. A client that pins that key cannot tell from the
   report whether TLS ends inside the TEE; that depends on how the deployment
   terminates TLS. The [E2EE v2 extension](../../spec/e2ee-v2.md) protects
   request content without relying on this. The fix is to publish custody
   evidence for the TLS key.

9. **The custody policy covers only the receipt key.** The dstack custody
   check matches the receipt key against the attested keyset, but never
   checks the E2EE custody entries against the published E2EE keys, and
   nothing covers the TLS key. The spec asks a custody policy to cover the
   receipt, E2EE and TLS keys (§3.3).

10. **Nothing limits how far ahead a keyset can expire.** The spec says a
    verifier should reject an expiry that is implausibly far away (§3.1). No
    verifier here does, and the gateway accepts any configured lifetime.
    Expiry is the only way to revoke keys without coordination (§3.4), so a
    keyset valid for decades cannot be revoked that way.

11. **The `aci-service` upstream verifier trusts `image_digest` without
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

12. **Session audits check the evidence's integrity, not what it proves.**
    Both verifiers check that a cited session hashes to its ID, that it was
    valid at the receipt's admission-time `served_at`, and that its evidence
    matches its digest (§9.2 checks 1 and 2). `pap` can also require specific claims with
    `--require-claim` (§9.2 check 3); the TypeScript verifier only displays
    them. Neither verifier re-checks what the evidence proves (§9.2 check 4).
    Possible next steps are a required-claims option for the TypeScript
    verifier and a way to reuse the provider verifiers to re-check evidence
    on the client.

## Tooling

13. **TypeScript CI does not run on spec-only changes.** The TypeScript
    verifier's tests compare against the spec's test vectors byte for byte,
    and its CI runs for changes under `clients/`. An edit to
    `spec/test-vectors.md` alone does not trigger it. The Rust test
    `tests/spec_vectors.rs` still catches the mismatch.

## Intentional differences

14. **Compatibility with dstack-vllm-proxy.** `/v1/attestation/report`,
    `/v1/signature/{id}`, and the `X-Signing-Algo` E2EE mode serve clients
    written before ACI and use their own report format. The k256 key they
    share with the E2EE v2 secp256k1 suite appears in the ACI keyset with its
    custody evidence; the legacy Ed25519 key stays outside ACI. As the spec
    requires (Appendix B), these routes never change ACI reports, receipts, or
    sessions.
