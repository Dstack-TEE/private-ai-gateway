# Reference Implementation vs. ACI Spec: Known Gaps

Where this implementation currently falls short of, or diverges from,
[the ACI Spec](../../spec/aci.md). The spec is authoritative; these are
implementation compromises, not spec changes. Each item is a candidate work
item.

## Verifier coverage

1. **The `aci` CLI has no custody profile.** §10.1(5) requires the verifier
   profile to check private-key custody (for this deployment, the dstack KMS
   signature chain in `attestation.evidence.key_custody`). The in-tree
   dstack chain validation (`src/aci/verifier/dstack.rs`) is not wired into
   the CLI, so `aci verify` reports L2.5 as an honest `skip`, never a pass.
   The top-line verdict and exit code do not distinguish a skip from a pass:
   a run can end `VERIFIED` (exit 0) with custody unevaluated. The skip and
   its reason are always printed in the transcript and counted in the
   verdict line; a relying party that requires §10.1(5) must gate on the
   L2.5 status, not on the exit code alone.

2. **Provenance is measured, not pinned.** When the service publishes
   `app_compose`, L2.4 verifies `sha256(app_compose)` equals the `compose-hash`
   measured into the quote's RTMR3; it does not pin the compose to an allowlist
   or rebuild `repo_url`/`repo_commit` from source. Services without
   `app_compose` skip L2.4 rather than pass — the transcript says which per run.

## Service conformance

3. **E2EE can be switched off per deployment.** With empty
   `supported_e2ee_versions` the gateway rejects E2EE requests
   (`src/http/app/handlers.rs`) and its keyset needs no §7.1 E2EE key
   (`AciService` enforces the §4.1 at-least-one-E2EE-key rule only when
   E2EE is advertised). Such a deployment is not spec-conformant — §1.4(5)
   requires E2EE on chat completions. Fine for dev; the launcher logs a
   startup warning when E2EE is disabled (`src/main.rs`).

4. **Receipts are in-memory only.** Receipt retention is bounded by
   `receipt_ttl_seconds` and lost on restart. The spec permits a bounded,
   implementation-defined retention period (§8.1), but a restart shortens it
   silently. Sessions do better: the JSONL store survives restarts and
   extends retention per citing receipt (§9 retention rule).

5. **Chutes per-instance sessions carry no §9.2 evidence.** The Chutes
   verifier's raw evidence is fleet-wide and nonce-bound, so sealing it into
   each per-instance session would mint a new session id for every
   verification round and every fleet change. The implementation instead
   seals per-instance sessions with an empty `evidence` object
   (`record_attested_upstream_session`), keeping them content-addressed on
   per-instance facts only. Consequence: the §10.3(4) deep-audit step fails
   closed on Chutes-cited sessions (`aci` CLI U.2; verifier-ts
   `checkSessionEvidence`) — Level-3 deep audit is unavailable for Chutes,
   while receipt verification and Level-1/2 checks are unaffected. The
   session store still accepts these records (its §9.2 check rejects only
   evidence whose `data` does not hash to `digest`). Candidate work item:
   have the provider verifier emit each instance's own evidence slice.

6. **Refusal receipts omit `request.forwarded`.** The §8.4 table marks the
   event required with no stated exception, but a §8.5 refusal (a failed
   `upstream.verified` accompanying an `upstream_verification_failed` error)
   never forwarded any bytes, so the reference implementation finalizes the
   refusal receipt without it (`ReceiptBuilder::finalize`). The in-tree
   verifiers do not require event presence; a strict generic verifier
   enforcing the table verbatim would reject refusal receipts.

7. **Streaming upstream errors carry no receipt.** A streaming request whose
   upstream answers non-200 is returned as a buffered error without a
   receipt (inherited dstack-vllm-proxy behavior,
   `forward_chat_completion_stream_request`), while the buffered path issues
   a receipt for the same upstream error status. Arguably outside §1.4(6) —
   no inference completed — but the coverage is asymmetric.

## Stale surroundings

8. **The live E2E suite predates the simplified protocol.**
   `docs/live-e2e-test-suite.md` and parts of `scripts/live_e2e/` (e.g.
   `cases/embeddings.py`, `cases/lifecycle.py`) still reference removed
   mechanism (legacy report fields, transparency events), as do
   `scripts/phala_multi_upstream_smoke.sh` and
   `scripts/local_multi_upstream_smoke.sh` (`workload_id`,
   `keyset_endorsement`). The in-process integration suites (`tests/`) cover
   the new protocol; the live scripts need the same pass, and the public
   deployment serves the previous build until redeployed.

9. **Client CI triggers are path-scoped.** `verifier-ts` tests pin the spec
   test vectors byte-for-byte, but the workflow triggers only on
   `clients/verifier-ts/**`, so an edit to `spec/test-vectors.md` alone does
   not rerun them (Rust CI catches drift via `tests/spec_vectors.rs`).

## Beyond-spec surfaces (intentional, keep honest)

10. **Legacy dstack-vllm-proxy compatibility** (spec §13).
    `/v1/attestation/report` (separate report-data layout, injected
    `signing_address` / `intel_quote` / `nvidia_payload`), `/v1/signature/{id}`,
    and the `X-Signing-Algo` E2EE mode serve pre-ACI clients. The k256 code
    they need is confined to that surface. The spec's rule that compatibility
    surfaces must not alter ACI artifacts holds: report, receipt, and session
    bytes are identical with or without compatibility parameters.
