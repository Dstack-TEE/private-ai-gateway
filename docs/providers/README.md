# Providers

One directory per upstream provider. Each holds up to two documents:

- **`verification.md`** — a living reference for **how the gateway verifies this provider
  and what cryptographically binds the session** it then enforces. Tracks the code.
- **`review.md`** — a point-in-time **admissions audit** against
  [`audit-criteria.md`](audit-criteria.md) (verdict, criteria status, required adapter
  changes, open questions).

| Provider | TEE | Session binding | Verification | Audit |
| --- | --- | --- | --- | --- |
| Chutes | Intel TDX + NVIDIA CC | `e2ee_public_key_sha256` | [verification](chutes/verification.md) | [review](chutes/review.md) |
| NEAR AI | Intel TDX + NVIDIA CC | `tls_spki_sha256` | [verification](near-ai/verification.md) | [review](near-ai/review.md) |
| Tinfoil | AMD SEV-SNP (or TDX) + NVIDIA CC | `tls_spki_sha256` | [verification](tinfoil/verification.md) | [review](tinfoil/review.md) |
| AciService (first-party) | Intel TDX + NVIDIA CC | `tls_spki_sha256` | [verification](aci-service/verification.md) | — (first-party) |
| PhalaDirect | Intel TDX + NVIDIA CC | `tls_spki_sha256` | [verification](phala-direct/verification.md) | [review](phala-direct/review.md) |
| SecretAI | AMD SEV-SNP + NVIDIA CC | — (adapter deferred) | — | [review](secret-ai/review.md) |

The two columns are different document *types* — `verification.md` tracks the running
code; `review.md` is a dated audit snapshot — so they are kept side by side rather than
merged. The framework and cross-cutting reviews:

- [`audit-criteria.md`](audit-criteria.md) — the admission framework, including
  criteria 13 (source & platform provenance) and 14 (platform TCB freshness).
- Source review lanes (router-mode providers):
  [router-mode-soundness.md](../reviews/router-mode-soundness.md),
  [router-mode-load-balancing-cache.md](../reviews/router-mode-load-balancing-cache.md),
  and the process in [router-mode-provider-review.md](../router-mode-provider-review.md).

## Prefix-cache tenant isolation

As observed on 2026-07-13, Private AI Gateway does not guarantee per-tenant
prefix-cache partitioning for the active Kimi-K2.6 providers. The gateway
preserves a caller's `cache_salt` but does not derive one from the authenticated
Redpill tenant.

- Tinfoil [replaces `cache_salt`](https://github.com/tinfoilsh/confidential-model-router/blob/v0.0.118/cache_salt.go)
  with a value derived from Redpill's shared upstream credential. The gateway
  does not set `user_cache_secret`, so Redpill tenants share one namespace.
- Chutes passes `cache_salt` to vLLM but does not generate it. Unsalted requests
  share the serving instance's namespace.

Tinfoil's behavior is attestation-backed. Chutes configuration is control-plane
evidence and is not bound by its current attestation. The intended interface is
caller-controlled: preserve `cache_salt` for Chutes and translate it to
`user_cache_secret` for Tinfoil. The gateway should not derive or override the
partition from Redpill tenant identity.

## The shared verification model

**A session binding is only trustworthy if it is bound into a verified attestation.**
Every provider produces exactly one kind of binding, and in every case the bound value
lives inside (or is digested into) the quote/report whose signature is verified. Each
`verification.md` states plainly *what is bound* and *what a tamper rejects*.

The two binding types:

- **`tls_spki_sha256`** — SHA-256 fingerprint of the upstream's TLS public key; the
  backend enforces it against the actual upstream HTTPS connection before forwarding.
- **`e2ee_public_key_sha256`** — SHA-256 of the upstream's end-to-end public key; the
  backend encrypts the request body to that key, so only the attested enclave can
  decrypt.

### Invariant: verified ⟹ enforceable binding

A "verified" result that carries no enforceable channel binding is rejected
(`src/aci/verifier.rs`). Forwarding fails closed if the selected backend cannot enforce
the accepted binding
(`tests/upstream_verifier.rs::service_fails_if_selected_backend_cannot_enforce_channel_binding`).
A provider can never be "verified but unpinned."

### The attested session record

When an upstream is verified, `record_attested_upstream_session`
(`src/aggregator/service.rs`) content-addresses the verified binding, verifier id,
target, and evidence digest into a stable `session_id` (`as_<sha256>`), stores an
`AttestedSession` served by `GET /v1/aci/sessions/{session_id}`, and attaches
that `session_id` to the receipt's `upstream.verified` event. Retention is the receipt
TTL — a retention window, not a binding-validity deadline (see
[../upstream-verification-lifecycle.md](../upstream-verification-lifecycle.md)).

### How a relying party verifies it end-to-end

1. `GET /v1/attestation/report?nonce=<random>` — verify the gateway's own ACI report.
2. `GET /v1/aci/receipts/{chat_id}` — verify the receipt signature under the attested keyset.
3. Read `upstream.verified.session_id`; `GET /v1/aci/sessions/{id}`.
4. Confirm the record's `target`, `verifier_id`, `evidence.digest`, and `session_binding`
   match the receipt event (nothing the middleware can forge).
5. The gateway has already enforced that binding on the wire before forwarding.

`scripts/live_e2e/user_verify.py` and `scripts/live_e2e/cases/attested_sessions.py`
implement this check; `verified_upstream_binding_creates_attested_session` covers record
creation.

A soundness pass (2026-06) tamper-tested every provider against its live upstream; each
`verification.md` records those results under "What a tamper rejects".
