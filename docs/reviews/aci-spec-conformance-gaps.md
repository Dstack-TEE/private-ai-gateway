# Reference Implementation vs. ACI Spec: Known Gaps

Where this implementation currently falls short of, or diverges from,
[the ACI Spec](../aci-spec.md). Spec soundness is authoritative; these are
implementation compromises and cleanups, not spec changes. None are fixed by
this document — each item is a candidate work item.

## Wire-format migrations required by the spec

1. **E2EE AAD format.** The spec defines the AAD as JCS canonical JSON with
   `aci.e2ee.request.v2` / `aci.e2ee.response.v2` purpose tags and field
   paths (spec §7.3). The implementation still emits the inherited
   pipe-delimited strings (`v2|req|algo=...`,
   `src/aggregator/service/e2ee_crypto.rs:88-140`) and rejects `|`/CR/LF in
   the model and nonce, which the JSON AAD makes unnecessary. Migrate the
   encoder/decoder and drop the ambiguity checks.

2. **Multimodal E2EE coverage.** The spec covers every content-bearing field
   (spec §7.2): image (`image_url.url`) and audio (`input_audio.data`)
   request parts, and response `message.audio.data`. The implementation
   decrypts only whole-string content and `text` parts
   (`src/aggregator/service/e2ee_crypto.rs:267-303`) and encrypts only
   `content` / `reasoning_content` / `text` / `data[].embedding`. Whole-content
   encryption already protects multimodal requests end to end; per-part
   image/audio ciphertexts and response audio encryption are not implemented.

3. **WebCrypto-native defaults.** The spec RECOMMENDS the X25519 E2EE suite
   (`x25519-aes-256-gcm-hkdf-sha256`, HKDF info `aci.e2ee.v2.x25519`) and
   Ed25519 receipt signing so browser clients can verify with the Web Crypto
   API alone (spec §7.1, §8.5). The implementation serves only the secp256k1
   E2EE suite (`src/aci/e2ee.rs:32-36`; the keyset's `ed25519` E2EE entry is
   the legacy X-Signing-Algo path, not an ACI suite) and signs receipts with
   a secp256k1 key only (`src/dstack.rs:307-329`). Add an X25519 E2EE key
   and an Ed25519 receipt key to the dstack provider and keyset.

4. **Receipt top-level `model`.** The spec adds a top-level `model` field to
   every receipt — the user-requested model, before any rewrite (spec §8.2) —
   so even a direct single-model service records what was asked for. The
   receipt builder (`src/aci/receipt.rs`, `Receipt` in `src/aci/types.rs`)
   has no such field today; only `upstream.verified.model_id` (the
   upstream-served model) is recorded. Add it to the type, the canonical
   value, and the builder.

5. **Keyset revocation statement.** The spec defines an identity-key-signed
   revocation statement (`purpose: aci.keyset.revocation.v1`) and requires a
   bounded `keyset_epoch.not_after` (spec §4.7). The implementation has no
   revocation signing/serving path, and pins `not_after: u64::MAX`
   (`src/main.rs:372`, see item 6). Add a revocation-statement signer that
   reuses the endorsement machinery (`sign_keyset_endorsement` analogue in
   `src/dstack.rs`) and a serving surface for it.

## Soundness gaps

6. **Keyset epoch is pinned, not managed.** The launcher hard-codes
   `keyset_epoch { version: 1, not_after: u64::MAX }` (`src/main.rs:372`).
   The spec requires a monotonically increasing version per keyset change and
   a bounded expiry (§4.2, §4.7). There is no rotation machinery yet; until
   there is, verifiers get no rollback protection from the epoch and no
   expiry-based exposure bound.

7. **E2EE replay cache is per-process.** `claim_e2ee_replay` uses an
   in-memory map (`src/aggregator/service/e2ee.rs:445`). Replicas sharing one
   workload identity each keep their own cache, so a captured request could
   be replayed against a different replica within the 300-second window. The
   spec permits an in-memory cache for v1; with shared-identity replicas the
   claim weakens and deserves either a shared cache or a spec-level caveat on
   replica deployments.

8. **E2EE accepts arbitrarily short nonces.** `validate_e2ee_nonce` only
   rejects empty/ambiguous values
   (`src/aggregator/service/e2ee_crypto.rs:13-26`). The spec says nonces
   SHOULD carry ≥128 bits of randomness; the service could enforce a
   16-character floor.

9. **E2EE `request.received.body_hash` is not client-reproducible.** The
   hash covers `serde_json::to_vec` of the decrypted payload
   (`src/aggregator/service/e2ee.rs:317`), so a client would have to
   reproduce that exact serialization to pre-verify it. The spec documents
   this honestly (§12), but echoing the observed body bytes or hashing a
   defined canonical form would make the request-side check usable.

## Stale code and doc rot

10. **`examples/verify_aci_artifacts.rs` reads fields receipts no longer
    carry.** It looks up `upstream.verified` fields `vendor` and
    `evidence.digest` (`examples/verify_aci_artifacts.rs:106-115`), but the
    event emits `provider` and deliberately omits inline `evidence`
    (`src/aci/receipt.rs:140-160`). The summary prints nulls. It also predates
    attested sessions: no `session_id` recomputation, no claims, no deep
    audit.

11. **Stale "returned as headers" comments.** `MiddlewareForwarded` /
    `MiddlewareStreamingForwarded` document `selected_route`, `session_id`,
    and `failed_attempts` as "returned to the caller as headers"
    (`src/aggregator/service/wire.rs:76-98`); no such headers are emitted
    anywhere. By design the receipt is the committed reference — fix the
    comments.

12. **`docs/attested-session-system.md` names purpose tags that do not
    exist.** It cites `aci.identity.v1` / `aci.receipt.v1` as
    domain-separation strings; the implemented tags are `aci.report_data.v1`
    and `aci.keyset.endorsement.v1` (`src/aci/types.rs:146-147`), and
    receipts intentionally carry no purpose tag.

## Confusing naming (pre-launch rename candidates)

13. **`provider` means two things.** On a session, `provider` is the
    operator's upstream config label (`src/aggregator/session.rs:217`); on the
    `upstream.verified` event the same information is called `upstream_name`
    while `provider` there is the verifier adapter type
    (`src/aci/receipt.rs:53-72`). The spec describes both faithfully, but one
    consistent pair (e.g. `upstream_name` + `provider_type`) across event and
    session would remove a real reader trap. Content-addressed session ids
    depend on the field name, so rename before launch or not at all.

## Beyond-spec surfaces (intentional, keep honest)

14. **Legacy dstack-vllm-proxy compatibility.** `/v1/attestation/report`
    (separate 64-byte report-data layout, injected `signing_address` /
    `intel_quote` / `nvidia_payload` / `all_attestations`), `/v1/signature/{id}`,
    and `X-Signing-Algo` E2EE modes. The legacy ECDSA path signs with the E2EE
    secp256k1 key (`src/dstack.rs:384-409`) — deliberate key reuse inherited
    from vllm-proxy, confined to the legacy surface and documented there. The
    spec's rule that compatibility surfaces must not alter ACI artifacts
    holds today.

15. **E2EE can be switched off per deployment.** With empty
    `supported_e2ee_versions` the gateway rejects E2EE requests
    (`src/http/app/handlers.rs:493`). Such a deployment is not
    spec-conformant (the spec requires E2EE on chat completions); fine for
    dev, worth a startup warning in production mode.
