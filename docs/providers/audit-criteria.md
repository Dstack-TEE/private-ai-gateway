# Provider Audit Criteria

This document defines the admission bar for adding an upstream provider to the
Private AI Gateway. It distills the NEAR AI, Tinfoil, and Chutes reviews,
the workload-identity work, and the implementation constraints we have agreed
on.

The goal is a strict but adoptable soundness check. A provider passes only when
there is no known gap in the path from verified workload identity to protected
user traffic. Unknowns are written as TODOs and block strict inclusion until
resolved.

Each provider can use provider-specific evidence and verifier code. The common
output to the gateway is always the same: a verified lease with enforceable
channel binding and enough provider-owned claims for receipts and audits.

## Core Principle

The provider must let Private AI Gateway establish a workload identity and then bind every
forwarded request to that identity.

For a direct model endpoint, the workload identity may be the model service
itself. For a gateway or router provider, the gateway can be the trust boundary
if its verified code and configuration enforce verification of the downstream
model path.

The gateway should not port every provider verifier into Rust. Provider
adapters may call provider-owned verifiers, local scripts, SDKs, or compact
Rust code. The Rust gateway consumes the verified lease and enforces the
transport binding.

## Required Criteria

### 1. Workload Identity

The provider must expose a workload identity that can be verified from public
or provider-supplied evidence.

Required evidence:

- hardware or workload attestation, or an equivalent confidential-computing
  proof accepted for that provider
- freshness, usually a nonce
- a stable identity key or identity claim
- source, build, image, or compose provenance for the verified workload

The identity may be a direct model instance, a provider gateway, or a provider
router. The review must state which one is the trust boundary.

The review must also state the key lifecycle. A public key returned next to a
quote is not enough. The private key must be generated inside the accepted
workload, sealed to it, released by an attestation-gated KMS, or otherwise
proved under provider-specific rules.

### 2. Channel Binding

The verified identity must be bound to the channel Private AI Gateway will use for real
requests.

Accepted examples:

- TLS SPKI hash bound into the attestation report
- E2EE public key bound into the attestation report
- provider-specific encrypted transport where the encryption key is part of
  the verified workload identity

The adapter must enforce the binding on the actual request path. Verifying a
provider once and then sending traffic over ordinary unpinned TLS is not
acceptable.

For normal HTTPS providers, TLS SPKI pinning is the baseline. Certificate
Transparency, CAA, WebPKI, and DNS ownership are useful operational signals,
but they do not replace SPKI or an equivalent attested transport binding.

### 3. Gateway Soundness

If the provider uses a gateway or router as the trust boundary, the review must
show that the gateway code enforces the trusted downstream path.

Required findings:

- how downstream providers are discovered
- when downstream attestation or key verification happens
- how the gateway pins or binds the downstream transport
- what happens when verification fails
- whether retries, refreshes, and reconnects can bypass verification
- whether non-TEE or external providers exist in the same codebase

Gateway mode is acceptable when the verified gateway can be trusted to make
model-scoped statements such as "this model currently has attested backend
evidence." In that case Private AI Gateway does not need to re-verify every nested
attestation artifact. The model-scoped statement must still be fetched through
the verified gateway channel during lease establishment.

Gateway mode does not mean "trust a provider API." It means "trust this
attested gateway workload, with reviewed source/provenance/runtime policy, to
verify and route to its downstream workloads."

### 4. Model Selection

The provider must make it possible to bind a verified lease to the model
Private AI Gateway will expose.

Required behavior:

- use canonical provider model ids in the lease
- reject or avoid models that cannot produce the provider's verification
  signal
- treat catalog flags as hints unless they are themselves signed or bound to
  the verified workload
- prevent aliases from bypassing the verified model identity

For gateway providers, a model-scoped probe is preferred over trusting static
catalog metadata. The NEAR AI rule is the reference example:

```text
verified gateway channel
+ /v1/attestation/report?model=<canonical-model>
+ non-empty model_attestations[]
```

The review must call out aliasing and actual served weights. If the provider
cannot prove the exact backend model or quantization, receipts must be honest
about the verified claim they can make and must not imply more.

### 5. Privacy Boundary

Plaintext prompts, completions, files, tool payloads, embeddings, and API keys
must stay inside the accepted trust boundary, or the provider must explicitly
be marked as not satisfying ACI privacy.

The audit must inspect:

- request and response logs
- metrics and traces
- billing events
- persistence and conversation storage
- file upload paths
- tool execution and MCP paths
- retries and error reporting
- debug and development modes

If a feature can carry plaintext outside the trust boundary, the adapter must
disable that feature, avoid that request shape, or mark the provider as
unacceptable for ACI-secured traffic.

### 6. Runtime Policy

The attested workload's runtime policy must be part of the review.

Examples:

- allowed image or compose hashes
- debug mode disabled
- strict TCB settings when required
- no off-TEE TLS termination
- credentials loaded only inside the intended trust boundary
- provider catalog update controls

Source provenance alone is not enough when security-critical behavior is
controlled by runtime config.

Runtime policy must cover both the public entry point and any downstream path
that sees plaintext. For router providers, policy includes the provider catalog
and admin controls that can add, remove, or retarget models.

### 7. Release And Measurement Updates

Strict inclusion requires an upgrade process, not only a point-in-time
measurement pin.

Providers should publish candidate release material before production rollout:

- source commit, release tag, or build provenance
- image digest, compose hash, workload id, or equivalent measured identity
- expected runtime policy and security-relevant config
- a description of security-relevant changes

The relying party should be able to review the candidate code/release and
compute or independently confirm the measurements before those measurements are
accepted in production. If a provider can switch to new measurements without
prior publication, the verifier can only choose between failing closed or
blindly trusting a new unreviewed workload.

Emergency rollouts can exist, but they must be explicit: the provider should
mark them as emergency measurements, publish the diff promptly, and expect
strict verifiers to reject them until reviewed unless the relying party has
opted into that emergency policy.

### 8. Lease Lifecycle

Provider verification should establish a lease, and request handling should be
lightweight.

A lease should include:

- provider id
- canonical model id
- verified workload identity
- channel binding
- evidence digest or compact provider claim
- verification time and expiry
- provider-specific session material if needed

Refresh must be non-destructive. A failed refresh should not replace a valid
old lease, but traffic must stop when the old lease expires.

Provider-specific session material cannot extend trust. For example, Chutes
E2EE nonces are usable only while their instance key is covered by the current
verification lease.

### 9. Request Fidelity

The provider must preserve the OpenAI-compatible behavior the gateway
exposes for that model.

Required checks:

- streaming and non-streaming response shape
- tool calls and structured outputs when advertised
- multimodal request paths when advertised
- context limits and cache metadata when advertised
- error handling and timeout behavior
- request and response rewriting transparency

The adapter must not hide provider incompatibility with response
post-processing that changes semantics. Unsupported features should be
explicitly disabled for that provider/model.

### 10. Load Balancing And Cache

The review must explain how the provider chooses among replicas.

Required findings:

- whether the trust boundary is per instance, router, or fleet
- whether the provider exposes per-instance endpoints
- whether the router preserves session, prompt-cache, or KV-cache affinity
- what backend identifier, if any, can be recorded in receipts
- whether retries can move a request after sensitive bytes were sent

Cache locality is not a security proof. Do not claim cache affinity unless it
is implemented and externally observable.

### 11. Receipts

Every forwarded request should be traceable to the verified lease.

Receipts should record:

- provider id and canonical provider model id
- lease or evidence digest
- channel-binding type and digest
- provider-specific claim summary
- request and response hashes
- transparency-log events for request or response rewrites

Receipts should not require users to understand every provider's native
attestation format. Detailed verifier evidence should use the common evidence
object: a `sha256:` digest over the decoded bytes and a data URI that preserves
the exact bytes and content type. If a verifier needs several upstream bodies,
use one `multipart/mixed` data URI rather than provider-specific JSON
gymnastics.

### 12. Negative Checks

The provider review or test suite must include fail-closed checks where
practical:

- mutate TLS SPKI, E2EE public key, key id, or origin and confirm forwarding
  fails
- request an unverifiable model and confirm lease establishment fails
- use an alias or catalog miss and confirm no unrelated model is selected
- expire or invalidate a lease and confirm traffic stops
- force a provider verifier failure and confirm the previous lease is not
  replaced
- exercise streaming error paths without losing receipt hashes

### 13. Source And Platform Provenance

A verified measurement only proves "some specific code/firmware is running."
Provenance proves *which* code, traced to reviewed, ideally reproducible source.
Two distinct layers must each be covered:

- **Software provenance** — the application/model/gateway code measured into the
  quote (compose hash, image digest, workload id, container measurement) maps to
  reviewed open source at a known commit/release, ideally via a reproducible build
  or a signed provenance attestation (e.g. Sigstore/in-toto with a transparency-log
  entry and a trusted builder identity), not merely "matches the provider's
  currently published value."
- **Platform/OS provenance** — the platform layer measured into the quote (guest
  OS image, kernel + command line, initramfs, bootloader, and the TEE firmware /
  module: TDX module `MR_SEAM`, SEV firmware/ucode level) maps to a reviewed,
  reproducible build or a documented known-good set. The OS and firmware are part
  of the TCB; an unreviewed OS image is as much a gap as unreviewed application
  code.

Both layers should be pinned through the release/upgrade process in criterion 7.
Where a layer is verified only for self-consistency (the measurement matches its
own reported hash) but not pinned to reviewed source, record it as a **TODO**; it
blocks strict inclusion.

### 14. Platform TCB Freshness

The verifier must read the platform TCB status (Intel TDX/SGX `TcbStatus`, AMD
SEV-SNP reported TCB against a minimum policy) and apply a documented, consistent
policy across providers — not silently accept any signed quote. A genuine TEE on
out-of-date microcode (`OutOfDate`) is still exposed to issues fixed in later TCB.

- Require `UpToDate`, or an explicit allowlist (e.g. `SWHardeningNeeded` /
  `ConfigurationNeeded`) that records the advisory IDs and the reason.
- Apply the same bar to every provider; document any deviation.
- Reject `OutOfDate` / `Revoked` unless the relying party has explicitly opted in.

## Hard Reject Conditions

Any one of these blocks strict inclusion:

- no enforceable channel binding on the request path
- plaintext user content can leave the accepted trust boundary
- a verified response can be produced by an unverified model path
- catalog metadata alone is treated as proof
- aliases can silently select an unrelated provider model
- debug/local bypasses can be enabled without changing pinned provenance
- new provider measurements can replace accepted measurements without prior
  publication or explicit emergency-review handling
- verifier failures fail open
- the provider cannot produce model-scoped evidence for the model being served
- the provider cannot support the advertised OpenAI-compatible surface without
  semantic post-processing

## Review Process

Each provider review should produce a `review.md` under `docs/providers/<provider>/`,
alongside the `verification.md` reference for that provider.

The reviewer should:

1. Identify the trust boundary: direct model, gateway, router, or E2EE
   instance.
2. Record source repositories, commits, release tags, and image/compose
   evidence.
3. Record how new releases and measurements are published before rollout.
4. Trace the serving path from public request to model backend.
5. Trace the attestation or verification path.
6. Trace the request transport binding.
7. Inspect catalog/model selection behavior.
8. Inspect plaintext egress paths.
9. Run live probes when practical.
10. Write positive evidence, concrete loopholes, required adapter behavior, and
   open questions.

The review should lead with one of four decisions:

- acceptable
- acceptable with conditions
- accepted for limited traffic
- not acceptable

`acceptable` means every required criterion is satisfied for the stated model
set and request surface. `acceptable with conditions` means the security model
passes but there are operational restrictions or required pins. `accepted for
limited traffic` means the security model passes but throughput, feature
coverage, or observability prevents general production use. `not acceptable`
means at least one hard reject condition remains.

## Minimal Adapter Contract

Every provider adapter should return the same class of result to Rust:

```json
{
  "result": "verified",
  "provider": "near-ai",
  "model_id": "canonical-model",
  "verifier_id": "provider-verifier/version",
  "evidence": {
    "digest": "sha256:...",
    "data": "data:application/json;base64,<exact-verifier-input-bytes>"
  },
  "verified_at": "2026-05-18T00:00:00Z",
  "expires_at": "2026-05-18T00:05:00Z",
  "channel_bindings": [
    {
      "type": "tls_spki_sha256",
      "origin": "https://cloud-api.near.ai",
      "spki_sha256": "..."
    }
  ],
  "provider_claims": {
    "trust_boundary": "gateway",
    "evidence_scope": "model",
    "gateway_verified": true,
    "model_evidence_present": true
  }
}
```

The exact `provider_claims` fields are provider-owned. Rust should enforce the
generic fields and the channel binding. Provider-specific meaning belongs in
the adapter and review document. When useful, adapters should include compact
readable scope facts such as `trust_boundary` and `evidence_scope`; the raw
proof input still belongs in `evidence.data`.

`evidence.digest` is computed over the decoded bytes from `evidence.data`.
Provider adapters must not rebuild evidence into a normalized JSON shape unless
that JSON is exactly the input fed to the verifier.

Provider verifiers may call provider-owned tools, local scripts, SDKs, or Rust
libraries. The verifier command itself is part of the provider adapter, not a
user-configurable policy hook.

## What We Should Avoid

Avoid adding unnecessary complexity to the protocol or gateway:

- no generic policy DSL
- no Rust port of every provider's native verifier unless it is clearly the
  simplest implementation
- no requirement that all providers expose identical attestation formats
- no per-request attestation when a verified lease and channel binding are
  sufficient
- no trust in catalog flags without a verified lease
- no upstream verification rules in the ACI core spec
- no provider-specific measurement pinning in generic dstack verifier config

The right abstraction is a provider adapter that establishes a verified lease.
The Rust gateway should stay small: select the lease, enforce the channel,
forward the request, and record the receipt.

## Current Provider Status

| Provider | Trust boundary | Decision | Blocking TODOs |
| --- | --- | --- | --- |
| NEAR AI | Verified gateway workload | Acceptable with conditions | Pin accepted gateway provenance/runtime policy; define pre-production measurement publication process; confirm no off-TEE TLS termination; keep model-scoped attestation probe mandatory; finish privacy/log/storage review. |
| Tinfoil | Verified confidential router | Acceptable with conditions | Pin audited router compose/image digest; define pre-production measurement publication process; decide runtime config update policy; record selected `Tinfoil-Enclave`; finish strict release pins. |
| Chutes | Verified E2EE model instances | Accepted for limited traffic | Pin exact `chute_id` or unique slug for production models; resolve nonce-throughput limit before general production; document served-model alias limitations. |
| SecretAI | Verified single SEV-SNP VM | Acceptable with conditions | Pin reviewed `artifacts_ver` allowlist (`secretvm-verify/.../sev.json`); pin `secret-ai-caddy` compose / image; confirm production journal stays inside the trust boundary; define SCRT pre-production measurement publication; treat TLS-cert renewal as forced lease refresh (binding is to cert SHA-256, not SPKI). |
