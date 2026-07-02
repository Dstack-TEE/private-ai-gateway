# Attested Confidential Inference (ACI) Specification

> **Version:** `aci/1` (draft)
> **Audience:** security researchers evaluating the protocol, and inference
> providers or aggregators implementing it.
> **Conformance language:** MUST, SHOULD, and MAY are used in the RFC 2119
> sense.
> **Reference implementation:** this repository. The implementation also
> carries compatibility surfaces inherited from dstack-vllm-proxy that are not
> part of this specification (§13).
> **License:** Apache License 2.0 (see `LICENSE`). The patent grant is
> intended: anyone may implement ACI, in any language, without further
> permission.

Attested Confidential Inference is an interface for AI inference services
whose clients want proof, not promises. An ACI service proves **what workload
is serving the API** with hardware-rooted TEE attestation, then binds every
later artifact — TLS sessions, encrypted fields, per-request receipts, and
upstream verification records — back to that proven workload.

ACI covers OpenAI-compatible inference endpoints and adds three verification
artifacts:

| Artifact | Endpoint | Question it answers |
| --- | --- | --- |
| Attestation report | `GET /v1/aci/attestation` | What workload and which keys serve this API? |
| Inference receipt | `GET /v1/aci/receipts/{id}` | What happened for this specific request? |
| Attested session | `GET /v1/aci/sessions/{session_id}` | Which verified upstream TEE served the inference (for aggregators)? |

ACI v1 deliberately does **not** define routing policy, billing, pricing,
model catalogs, canonical model identifiers, or a universal trust policy.
It standardizes bindings; each relying party chooses the verifier policy it
trusts (§1.3). For how ACI relates to other confidential-inference systems
and standards, see [ACI and Related Work](aci-related-work.md).

## 1. Trust Model

ACI establishes two claims:

1. **Privacy.** Plaintext prompts and outputs are visible only inside
   workloads the relying party has verified and accepted.
2. **Integrity.** Responses are bound to the exact request bytes, to any
   service-side transformation, and to attested code.

A verifier accepts these claims by checking hardware-rooted TEE evidence, the
binding of the workload identity and keyset into that evidence, source
provenance, freshness, and private-key custody. Attestation establishes *who
is serving*; receipts establish *what happened* per request; attested
sessions establish *which upstream* an aggregator trusted with the prompt.

### 1.1 What a client must check

If plaintext HTTPS terminates outside the accepted workload, a valid WebPKI
certificate provides no ACI assurance. A channel is ACI-verifiable only when
it is bound to the attested keyset:

- **TLS** is ACI-verifiable only when the observed server certificate's SPKI
  digest is listed in the keyset's `tls_public_keys`.
- **E2EE** is ACI-verifiable only when the service E2EE key is listed in
  `e2ee_public_keys`.
- **Receipts** are ACI-verifiable only when signed by a key listed in
  `receipt_signing_keys`.

Ordinary OpenAI SDK clients that check nothing get WebPKI assurance, not ACI
assurance. Such clients can gain ACI assurance through a verifier SDK, an
agent runtime, or a local verifying proxy.

SPKI pinning against the attested keyset is the required baseline because it
works with ordinary HTTPS stacks. Attested-TLS mechanisms that bind evidence
into the connection itself (the direction of the IETF SEAT working group)
MAY serve as a stronger transport profile once standardized; they do not
replace the baseline.

### 1.2 Aggregators

An aggregator is an ACI service that forwards inference to upstream
services. The aggregator is itself the client-facing workload: it proves its
own identity to clients exactly like a single-model service.

For the upstream hop, ACI v1 standardizes the aggregator's **transparency
surface**, not its routing policy:

- Before forwarding a prompt, the aggregator MUST verify the selected
  upstream and obtain an enforceable channel binding (a TLS key pin or an
  upstream E2EE key). If verification is required and fails, the aggregator
  MUST NOT forward the prompt (fail closed).
- Each receipt records the verification outcome in an `upstream.verified`
  event (§8.4).
- Each successful verification is captured as an immutable, content-addressed
  **attested session** (§9) that a verifier can fetch and re-check.

How the aggregator verifies a given upstream (which quote formats, which
measurements, which provenance) is verifier-specific and out of scope; the
recorded claims are honest about their source (§9.3), so a relying party can
distinguish hardware-proven facts from provider assertions.

### 1.3 Verifier profiles

An ACI service publishes one report plus evidence. It does not negotiate
trust. The relying party selects a **verifier profile** — a concrete
composition of TEE quote verification, source-provenance policy, key-custody
checks, and any platform-specific checks (for example dstack KMS validation).
A report is accepted if a profile the relying party trusts verifies it
completely.

A profile MUST define where each piece of required evidence comes from:
inline in the report, digest-bound and fetched from a profile-defined
location, directly observed by the verifier, or supplied by local policy.
Missing required evidence is fail-closed. A profile MAY add checks; it MUST
NOT relax the minimum checks in §10.

In IETF RATS terms (RFC 9334): the ACI service is the Attester, the
attestation report carries Evidence, the relying party appraises it
directly or through a Verifier it trusts, and the verifier profile is its
appraisal policy. ACI's typed session claims (§9.3) play the role of
attestation results with explicit provenance, in an inference-specific
vocabulary comparable to the RATS AR4SI trustworthiness vectors.

### 1.4 Conformance summary

An ACI-conformant service MUST:

1. Run the client-facing workload inside a TEE with hardware-rooted
   attestation.
2. Publish its attestation report at `GET /v1/aci/attestation`, binding
   `workload_id`, `workload_keyset_digest`, and the client nonce into the
   TEE evidence (§4.4, §5).
3. Endorse the current keyset with the identity key (§4.3).
4. Publish source provenance connecting the attested workload to public
   code or build artifacts (§5.1).
5. Keep every listed private key in TEE custody (§4.5), and bind any
   plaintext-HTTPS endpoint's TLS key into the keyset (§4.2).
6. Support E2EE on `POST /v1/chat/completions`, non-streaming and
   streaming (§7).
7. Compute receipt hashes inside the TEE from observed bytes, sign receipts
   with an attested key, and serve them at `GET /v1/aci/receipts/{id}` (§8).

An aggregator MUST additionally:

8. Verify each upstream and enforce a channel binding before forwarding a
   prompt, failing closed when required verification fails (§1.2).
9. Record the outcome in the receipt's `upstream.verified` event (§8.4) and
   publish attested sessions at `GET /v1/aci/sessions` (§9).

An ACI client (a verifier SDK, agent runtime, or verifying proxy acting for
the end user) MUST:

10. Establish the workload identity (§10.1) — itself, or through a Verifier
    it trusts — before releasing sensitive data.
11. Send sensitive data only over channels bound to the attested keyset: a
    pinned TLS SPKI or an attested E2EE key (§1.1).
12. Use fresh randomness where the protocol binds it: the attestation
    `nonce`, and a unique E2EE nonce per request (§7.5).

An ACI verifier MUST implement at least the §10.1 checks for the profile it
applies and fail closed on missing required evidence (§1.3).

## 2. Core Terms

- **ACI service** — a service implementing this protocol.
- **Aggregator** — an ACI service that forwards inference to upstream
  services.
- **Upstream** — a service an aggregator selects to perform inference.
- **Workload identity** — the stable identity public key (plus an optional
  profile-interpreted subject) that names a workload.
- **Workload keyset** — the document listing the workload identity, a keyset
  epoch, and the current operational public keys (receipt signing, E2EE,
  TLS).
- **Keyset endorsement** — the identity key's signature over the keyset
  digest.
- **Attestation statement** — the canonical payload, hashed into the TEE
  quote's report data, that binds `workload_id`, `workload_keyset_digest`,
  and a client nonce.
- **Attestation report** — the service's current evidence for its identity
  and keyset.
- **Inference receipt** — a signed per-request event log.
- **Attested session** — an immutable, content-addressed record of one
  verified upstream TEE channel.
- **Replica workload** — one of several functionally indistinguishable
  instances intentionally sharing one workload identity.

## 3. Canonicalization and Artifact Conventions

Every digest and signature payload in ACI is computed over **JCS** — the
JSON Canonicalization Scheme of RFC 8785 — applied to the object described.
Digest strings use the form `sha256:<lowercase-hex>`. Raw byte hashes (of
HTTP bodies, evidence bytes) use plain SHA-256 with the same string form.
[Test vectors](aci-test-vectors.md) pin every construction byte-for-byte.

ACI objects restrict JSON numbers to integers (versions, timestamps,
indexes). Implementations MAY therefore use a JCS subset that rejects
non-integer numbers rather than implementing ECMAScript number formatting; a
conformant ACI object never contains one.

Domain separation: every payload that is hashed into hardware evidence or
signed by the identity key carries an explicit `purpose` string
(`aci.report_data.v1`, `aci.keyset.endorsement.v1`). Receipt signing needs no
purpose string because receipt keys sign nothing else (§4.6).

### 3.1 Self-describing artifacts

ACI artifacts are built to be archived, forwarded, and verified later —
possibly by someone other than the original caller. Each artifact therefore
names its own verification context instead of assuming out-of-band
knowledge, even where a field is derivable from another:

- A receipt carries `workload_keyset_digest` — the key-resolution context.
  Across key rotations this is what tells a verifier *which* keyset document
  resolves `signature.key_id`. It also carries the stable `workload_id` as
  its issuer claim (the role `iss` plays in a JWT), so a reader can
  attribute an archived receipt without first fetching the keyset.
- A report carries top-level `workload_id` and `workload_keyset_digest` so a
  relying party can identify and cache it before hashing the keyset.
- `signature.algo` restates the algorithm of the keyset entry that `key_id`
  names.

Derivable fields are self-description, not trust: a verifier MUST check
each against recomputation (`workload_id` against the keyset's identity key,
the digest against the keyset, `signature.algo` against the named keyset
entry — the attested key decides the algorithm, never the artifact). Every
duplicated field sits inside signature or quote coverage, so it cannot be
altered independently without detection.

### 3.2 Extension points

The top-level fields of the receipt and the session record are **fixed** in
`aci/1`: implementations MUST NOT add top-level fields. Receipt signatures
cover the whole canonical object, and session ids are recomputed from named
fields, so unrecognized top-level content would either break closed-schema
verifiers or ride along unauthenticated. Extensions live at designated
points:

- **Receipts** — new event types, and new fields on existing events (§8.3).
  A verifier MUST preserve unknown events and event fields when recomputing
  the canonical signing bytes, and MUST otherwise ignore them unless local
  policy assigns them meaning.
- **Session records** — the `claims.extra` map (§9.3). Fields outside the
  defined record shape are not covered by the content id and MUST be
  ignored.
- **Reports** — inside `attestation.evidence` (profile-defined) and by new
  `service_capabilities` members, which consumers MUST ignore when
  unrecognized. The report is not signed as one object; its integrity comes
  from the per-field bindings of §5.1.
- **The keyset** is fixed; new key roles or fields require a new protocol
  version.
- New values for enumerated identifiers are governed by Appendix A.

## 4. Workload Identity

The identity model is one long-lived keypair plus one rotating key
document, tied together by two hashes:

```text
        TEE hardware root of trust
                  │  signs
                  ▼
         attestation quote
   report_data = sha256(JCS(attestation_statement))
                  │  binds
     ┌────────────┴──────────────┐
     ▼                           ▼
 workload_id            workload_keyset_digest
 hash of the            hash of the workload keyset,
 identity public key    endorsed by the identity key
     │                           │  lists
     │                           ▼
     │            receipt signing keys · E2EE keys · TLS SPKIs
     │                           │  verify
     └── stable name ──►  receipts · encrypted fields · TLS sessions
```

A verifier checks the quote once. After that, every receipt, encrypted
field, and TLS connection can be checked offline against keys in the
attested keyset.

### 4.1 Identity key and `workload_id`

A workload has exactly one stable identity public key:

```json
{ "algo": "ed25519" | "ecdsa-secp256k1" | "<other>", "public_key": "<hex>" }
```

Its public identifier is the hash of that key object:

```text
workload_id = "sha256:" || hex(sha256(JCS(workload_identity.public_key)))
```

`workload_id` is stable across operational key rotation: rotating receipt,
E2EE, or TLS keys never changes it. Only replacing the identity key itself
creates a new workload identity; ACI v1 defines no continuity across that
event.

The identity object MAY also carry a `subject` — naming metadata such as a
dstack app-id URI, SPIFFE ID, or DNS name, interpreted only by verifier
profiles. `subject` is not part of `workload_id`; generic verifiers MUST NOT
trust it by itself.

### 4.2 Workload keyset

The keyset is the single document listing everything the workload can
currently do:

```json
{
  "workload_identity": {
    "public_key": { "algo": "ecdsa-secp256k1", "public_key": "<hex>" },
    "subject": "<string-or-null>"
  },
  "keyset_epoch": { "version": 1, "not_after": 1790000000 },
  "receipt_signing_keys": [
    { "key_id": "<stable-id>", "algo": "ecdsa-secp256k1" | "ed25519", "public_key": "<hex>" }
  ],
  "e2ee_public_keys": [
    { "key_id": "<stable-id>", "algo": "x25519-aes-256-gcm-hkdf-sha256" | "secp256k1-aes-256-gcm-hkdf-sha256" | "<other>", "public_key": "<hex>" }
  ],
  "tls_public_keys": [
    { "spki_sha256": "<hex>", "domain": "<optional-hostname>" }
  ]
}
```

```text
workload_keyset_digest = "sha256:" || hex(sha256(JCS(workload_keyset)))
```

Rules:

- `e2ee_public_keys` MUST contain at least one client-facing ACI E2EE key
  (§7). Entries with other `algo` values MAY be present (for example
  compatibility keys); clients select by `algo`.
- `tls_public_keys` is required for services accepting sensitive plaintext
  over HTTPS. The digest is over the certificate SPKI, not the whole
  certificate, so renewals that keep the TLS key do not rotate the keyset.
  An entry MAY carry a `domain` restricting it to one public hostname; a
  client MUST pin the SPKI listed for the hostname it connects to.
- `keyset_epoch.version` MUST increase with every keyset change for a given
  `workload_id`; stateful verifiers SHOULD reject rollback.
  `keyset_epoch.not_after` is a Unix timestamp after which verifiers MUST NOT
  accept the keyset for new TLS, E2EE, or receipt verification.
- Operational keys MUST be distinct per role: a receipt signing key MUST NOT
  double as an E2EE key or TLS key.

Any change to the keyset — a rotated key, a changed subject, a new epoch —
produces a new `workload_keyset_digest`, a new endorsement, and a fresh
attestation report binding the new digest. There is no soft-rotation path
that changes keys without fresh attestation. Historical receipts keep
referencing the digest that was current when they were signed.

### 4.3 Keyset endorsement

The identity key signs the keyset digest, under a purpose tag:

```text
keyset_endorsement_payload = JCS({
  "purpose": "aci.keyset.endorsement.v1",
  "workload_keyset_digest": workload_keyset_digest
})
```

`keyset_endorsement.value` is the hex-encoded signature over those bytes by
the identity private key:

- `ed25519` — a 64-byte RFC 8032 signature over the payload bytes.
- `ecdsa-secp256k1` — a 64-byte `r || s` signature over
  `sha256(payload bytes)`.

### 4.4 Attestation binding

The hardware quote binds the identity, the current keyset, and the client's
freshness challenge:

```text
attestation_statement = {
  "purpose": "aci.report_data.v1",
  "workload_id": workload_id,
  "workload_keyset_digest": workload_keyset_digest,
  "nonce": <string-or-null>
}

report_data = sha256(JCS(attestation_statement))
```

`nonce` is the URL-decoded UTF-8 value of the `nonce` query parameter of the
report request, or JSON `null` when omitted (never the string `"null"`).

Verifier profiles define how the 32-byte `report_data` value is placed into
the native TDX / SEV-SNP report-data slot (padding, position); they MUST NOT
change the digest calculation.

The quote and the endorsement are complementary and both required. The quote
proves the endorsed keyset was active inside the measured workload at quote
time; the endorsement proves the identity key holder stands behind that
keyset. A verifier MUST NOT accept keys that appear next to a quote but are
not bound through both the report-data calculation and the endorsement.

### 4.5 Key custody

Public-key binding is worthless without private-key custody. A service MUST
NOT list a public key in the keyset unless the corresponding private key is:

- generated inside the attested workload, or
- sealed exclusively to it, or
- released to it only after successful attestation of an equivalent workload
  (for example by an attestation-gated KMS).

Verifier profiles MUST specify how custody is checked for the identity,
receipt, E2EE, and TLS keys — for example by validating a KMS signature
chain published in the report's evidence.

Multiple replicas MAY share one workload identity when they are functionally
indistinguishable to clients and each replica independently satisfies the
attestation and key-release requirements. The key-distribution protocol for
replicas is out of scope.

### 4.6 Design notes (informative)

- ACI attests one keyset epoch, not every derived artifact. Receipts are per
  request and signed by an attested key; hashing each receipt into
  `report_data` would require a quote per inference.
- `report_data` binds only key state and the nonce. Provenance, capabilities,
  and freshness metadata are excluded: they are already covered by TEE
  measurements and evidence, or are verifier-local concerns.
- Signature contexts do not overlap: the identity key signs only endorsement
  and revocation payloads (each with its own purpose tag), and receipt keys
  sign only receipts. The per-role key separation in §4.2 is what keeps this
  sound.
- Algorithm defaults target clients without special cryptography: with the
  X25519 E2EE suite and Ed25519 signatures, a browser can verify every ACI
  artifact using only the Web Crypto API (SHA-256, HKDF, AES-GCM, Ed25519,
  X25519). The secp256k1 options carry the EVM/dstack ecosystem — on-chain
  `ecrecover` verification and KMS-derived keys — without putting that
  ecosystem's toolchain in every client's path. P-256 was considered and
  left out to keep the matrix small; an extension can add it if HSM-custody
  profiles require it.
- Keys and signatures use a minimal bespoke encoding rather than JWK/JWS.
  Every signed object in ACI already rides on JCS, so one canonicalization
  covers keys, statements, receipts, and AAD; wrapping signatures in JOSE
  would add a second framework — with its algorithm-agility and
  header-confusion pitfalls to profile away — without removing the JCS
  dependency. `workload_id` is the same construction as an RFC 7638 JWK
  thumbprint, applied to the ACI key object. A JOSE binding (JWK export of
  keyset entries, JWS receipt envelopes) can be layered on as an extension
  without changing the trust chain.

### 4.7 Expiry and revocation

A workload identity or keyset can be compromised. ACI answers this in three
layers, from the cheapest to the strongest, so a deployment can rely on the
first and reach for the others as its threat model requires.

**Bounded lifetime.** Every keyset MUST set a bounded
`keyset_epoch.not_after`, and a verifier profile SHOULD reject an
implausibly distant expiry. Because recency comes from the nonce (§5.1) and clients always
re-fetch the report, an expired keyset stops producing acceptable reports on
its own — this is the baseline bound on how long a compromise stays useful,
and it needs no coordination. A shorter window trades more frequent
re-attestation for a smaller exposure window.

**Graceful rotation.** To replace a keyset ahead of expiry, the service
publishes a new epoch with a higher `keyset_epoch.version` and a fresh report
(§4.2). Stateful verifiers SHOULD reject the superseded version (the §4.2
rollback rule), so rotation needs no separate signal.

**Explicit revocation.** To repudiate a keyset immediately — for example when
an operational key leaked but the identity key remains in separate custody —
the identity key signs a revocation statement:

```text
keyset_revocation_payload = JCS({
  "purpose": "aci.keyset.revocation.v1",
  "workload_keyset_digest": <revoked digest>
})
```

A service MUST stop serving a revoked keyset, and a verifier that obtains a
valid revocation MUST reject reports and receipts under that digest. The
statement verifies exactly like the endorsement (§4.3), under the identity
key. This does not help when the **identity** key itself is compromised: no
in-band signal from a key the attacker controls is trustworthy.

**Relying-party deny-list.** The backstop for identity-key compromise, or for
revoking faster than clients re-fetch, is a relying-party policy decision, not
an in-band protocol message. ACI's contribution is the stable identifiers a
deny-list keys on — `workload_id` and `workload_keyset_digest`. How such a
list is published and distributed (an operator revocation endpoint, a
transparency log, an on-chain registry) is profile- and deployment-specific
and out of core scope. Archival verification of receipts signed under a
now-expired or revoked keyset is likewise a local-policy decision (§12).

## 5. Attestation Report

```text
GET /v1/aci/attestation?nonce=<fresh-client-nonce>
```

Returns the service's current attestation report. The endpoint is
service-scoped: one report describes the whole workload, not one model.
Clients SHOULD supply a fresh random `nonce` and check it is bound into
`report_data`.

### 5.1 Response

```json
{
  "api_version": "aci/1",
  "workload_id": "sha256:<hex>",
  "workload_keyset_digest": "sha256:<hex>",
  "attestation": {
    "vendor": "<operator-label>",
    "tee_type": "tdx" | "sev_snp" | "<other>",
    "workload_keyset": { "...": "keyset from §4.2" },
    "report_data": "<hex>",
    "keyset_endorsement": { "algo": "<identity-key-algo>", "value": "<hex>" },
    "source_provenance": {
      "repo_url": "<https-url-or-null>",
      "repo_commit": "<git-commit-or-null>",
      "image_digest": "<sha256-prefixed-digest-or-null>",
      "image_provenance": { "...": "..." } | null
    },
    "freshness": { "fetched_at": 1750000000, "stale_after": 1750003600 },
    "evidence": { "...": "TEE-type-specific evidence" }
  },
  "service_capabilities": {
    "supported_e2ee_versions": ["2"]
  }
}
```

Field rules:

- `workload_id` MUST equal the §4.1 digest of
  `attestation.workload_keyset.workload_identity.public_key`, and
  `workload_keyset_digest` MUST equal the §4.2 digest of
  `attestation.workload_keyset`.
- `keyset_endorsement` MUST verify under the identity public key with the
  §4.3 payload, and its `algo` MUST match the identity key's `algo`.
- `report_data` MUST equal the §4.4 statement digest for the requested nonce,
  and the TEE evidence MUST bind that value.
- **Source provenance** MUST let an independent verifier connect the attested
  workload to public code or build artifacts: at least `repo_url` plus
  `repo_commit`, or `image_digest`. A launcher-based profile MAY satisfy this
  by proving that an attested, provenance-checked launcher fetched and ran a
  pinned commit. A report without acceptable provenance MUST be rejected by
  the verifier (the wire field may be absent on non-conformant or development
  deployments).
- **Freshness**: recency comes primarily from the **nonce** — a client that
  supplied a fresh `nonce` and checks it is bound into `report_data` knows
  the quote was produced after the challenge. `fetched_at` / `stale_after`
  are the service's declared validity window, meaningful to a verifier only
  when its profile trusts the platform's clock; a profile that relies on
  them SHOULD require a securely synchronized TEE clock (TDX and SEV-SNP
  expose a trusted clock, but its accuracy depends on secure time sync such
  as authenticated NTP). Absent that, treat the timestamps as advisory and
  rely on the nonce. A report is never valid past
  `keyset_epoch.not_after` (§4.7).
- `service_capabilities.supported_e2ee_versions` lists the client-facing ACI
  E2EE scheme versions the service terminates (this document defines `"2"`,
  §7). Upstream-only encryption schemes MUST NOT be advertised here.

### 5.2 Evidence

`tee_type` selects the evidence format: `tdx` means Intel TDX quote
verification, `sev_snp` means AMD SEV-SNP report verification, and any other
value requires a published verifier extension. The `evidence` object is
interpreted by the verifier profile.

As an informative example, the reference implementation's dstack/TDX profile
publishes:

```json
{
  "quote": "<hex TDX quote>",
  "quote_report_data": "<hex report-data bytes bound by the quote>",
  "event_log": [ "...boot / RTMR event log..." ],
  "vm_config": { "...VM and TCB configuration..." },
  "key_custody": { "provider": "dstack-kms", "keys": [ "...KMS signature chains..." ] },
  "downstream_tls_binding": { "domain": "<host>", "spki_sha256": "<hex>" }
}
```

When the keyset contains domain-scoped TLS entries, the report MUST be
requested through a hostname the keyset knows, so the client pins the SPKI
for the hostname it actually uses.

## 6. Inference Endpoints

ACI v1 covers OpenAI-compatible completion-style endpoints. Request and
response bodies follow the OpenAI API; ACI adds headers and artifacts, not
body fields.

| Endpoint | Status |
| --- | --- |
| `POST /v1/chat/completions` | REQUIRED |
| `POST /v1/completions` | OPTIONAL |
| `POST /v1/embeddings` | OPTIONAL (non-streaming only) |
| Other completion-style endpoints (e.g. Anthropic-format `/v1/messages`) | OPTIONAL |
| `GET /v1/models` | OpenAI-compatible; ACI adds no required fields |

Trust metadata is service-level and lives in the attestation report. Clients
MUST NOT infer trust from `/v1/models` entries.

### 6.1 Request headers

| Header | When | Meaning |
| --- | --- | --- |
| `Authorization: Bearer <key>` | inherited | Service authentication. Also binds the receipt to this credential (§8.6). |
| `X-E2EE-Version: 2` | E2EE | E2EE scheme version; this document defines `2`. |
| `X-Client-Pub-Key` | E2EE | Client public key (hex, same curve as the selected suite) that response fields are encrypted to. |
| `X-Model-Pub-Key` | E2EE | The service E2EE public key the client selected from the attested keyset. |
| `X-E2EE-Nonce` | E2EE | Unique request nonce (§7.5). |
| `X-E2EE-Timestamp` | E2EE | Unix seconds (§7.5). |
| `X-Upstream-Verification: required \| none` | aggregator, optional | Default `required`: fail closed if the upstream cannot be verified. `none` lets this request proceed without upstream verification. Any other value is rejected. |

### 6.2 Response headers

| Header | When | Meaning |
| --- | --- | --- |
| `X-ACI-Version: aci/1` | every response | Protocol version, including error responses. |
| `X-ACI-Identity` | every response | The serving `workload_id`. |
| `X-ACI-Keyset-Digest` | every response | The serving `workload_keyset_digest`. |
| `X-Receipt-Id` | inference responses | Lookup id for the signed receipt. |
| `X-E2EE-Applied: true \| false` | inference responses | Whether response fields are E2EE-encrypted. |
| `X-E2EE-Version`, `X-E2EE-Algo` | when E2EE applied | Version and algorithm used. |

Headers are unauthenticated routing hints. A changed `X-ACI-Identity` means a
different workload; a changed `X-ACI-Keyset-Digest` means key rotation under
the same identity. Either way the client SHOULD re-fetch and re-verify the
attestation report before sending further sensitive data. The authenticated
bindings are always the attested keyset and the signed receipt, never the
headers.

## 7. End-to-End Encryption (E2EE)

E2EE encrypts the content-bearing request and response fields between the
client and the attested workload, on top of TLS. It exists so that clients
can bind their plaintext to a key proven to live inside the TEE even when
TLS terminates elsewhere (load balancers, CDNs), and so the decryption
capability itself is attested.

A service advertising E2EE MUST support it on `POST /v1/chat/completions`
for both non-streaming and streaming responses, and SHOULD support it on the
other completion-style endpoints it serves. `X-E2EE-Version` selects the
E2EE scheme; this document defines version `2` (lower values are reserved by
historical implementations and are not part of ACI).

### 7.1 Algorithms

ACI v1 defines two cipher suites. Both use ECDH between a fresh ephemeral
key and the recipient's static key (the service key from the attested
keyset for requests; the client's `X-Client-Pub-Key` for responses),
HKDF-SHA256, and AES-256-GCM — they differ only in the curve:

| `algo` | Curve | Ephemeral key encoding | HKDF `info` |
| --- | --- | --- | --- |
| `x25519-aes-256-gcm-hkdf-sha256` | X25519 | 32 bytes raw | `aci.e2ee.v2.x25519` |
| `secp256k1-aes-256-gcm-hkdf-sha256` | secp256k1 | 65 bytes, uncompressed SEC1 | `aci.e2ee.v2.secp256k1` |

The X25519 suite is RECOMMENDED: every primitive in it is available in the
Web Crypto API of current browsers and in every mainstream standard
library, so clients need no third-party cryptography. The secp256k1 suite
serves clients in the EVM/dstack ecosystem, where that curve is the native
toolchain. A service MUST publish at least one suite in
`e2ee_public_keys` and SHOULD publish the X25519 suite; the client selects
a suite by the `algo` of the keyset entry it encrypts to.

The AES-256-GCM key is derived as:

```text
key = HKDF-SHA256(salt = none, ikm = ecdh_shared_secret, info = <suite info string>, len = 32)
```

where `ecdh_shared_secret` is the raw X25519 output or the x-coordinate of
the secp256k1 shared point. Each encrypted field value is the lowercase-hex
encoding of:

```text
ephemeral_public_key || aes_gcm_nonce (12 bytes) || ciphertext || tag (16 bytes)
```

A fresh ephemeral key and AES-GCM nonce MUST be used per encrypted field.
Public keys are hex, with an optional `0x` prefix; for secp256k1, the
64-byte uncompressed form without the `0x04` prefix MUST be accepted and
treated as the same key.

### 7.2 Encrypted fields

The client encrypts field values in place; the JSON structure stays
OpenAI-compatible. E2EE covers every content-bearing field — text, images,
audio — not only text.

Each encrypted location is named by its **field path**: the JSON member
names and array indexes from the body root, joined with `.` — for example
`messages.3.content`, `messages.1.content.0.image_url.url`,
`choices.0.message.content`, `data.4.embedding`. For `choices` and `data`
entries the index is the entry's `index` member (its array position when
absent); all other array indexes are positional. The field path appears in
the AAD (§7.3), so a ciphertext cannot be moved to another location.

Request locations:

| Content | Field path |
| --- | --- |
| whole message content, any modality | `messages.{m}.content` — the content value (a plain string, or a structured content array serialized to JSON) encrypted as one ciphertext |
| text part | `messages.{m}.content.{c}.text` |
| image part | `messages.{m}.content.{c}.image_url.url` |
| audio part | `messages.{m}.content.{c}.input_audio.data` |
| completion prompt | `prompt`, or `prompt.{i}` per string element |
| embedding input | `input`, or `input.{i}` per string element |

Rules:

- The client SHOULD encrypt every content-bearing field it sends. For part
  types not listed above, the client MUST use whole-content encryption
  (serialize the content array to JSON and encrypt it at
  `messages.{m}.content`) — the universal form that covers any modality.
- A decrypted whole-content plaintext that parses as a JSON array is
  restored as structured content (an array of parts); anything else is used
  as a plain string.
- A request MUST contain at least one encrypted field, or it is rejected
  with `e2ee_decryption_failed`.
- Non-string array elements (for example token-id arrays in `input`) pass
  through unencrypted.

Response locations — the service MUST encrypt every generated-content field
present in the response:

| Endpoint | Buffered | Streaming (per SSE chunk) |
| --- | --- | --- |
| chat-style | `choices.{i}.message.content`, `choices.{i}.message.reasoning_content`, `choices.{i}.message.audio.data` | `choices.{i}.delta.content`, `choices.{i}.delta.reasoning_content` (an empty-string delta content MAY be dropped instead of encrypted) |
| `/v1/completions` | `choices.{i}.text` | `choices.{i}.text` |
| `/v1/embeddings` | `data.{i}.embedding` (the JSON value serialized compactly, then encrypted) | — (buffered only) |

### 7.3 AAD

Every ciphertext is bound to its location and request context through the
AES-GCM associated data. The AAD is the JCS canonicalization (§3) of a
purpose-tagged object — the same canonical form used everywhere else in
ACI, so no component needs escaping rules:

```text
request field:
  aad = JCS({
    "purpose": "aci.e2ee.request.v2",
    "algo":    <service E2EE key algo>,
    "model":   <request model>,
    "field":   <field path>,
    "nonce":   <X-E2EE-Nonce>,
    "ts":      <X-E2EE-Timestamp, integer>
  })

response field:
  aad = JCS({
    "purpose": "aci.e2ee.response.v2",
    "algo":    <service E2EE key algo>,
    "model":   <request model>,
    "id":      <response id>,
    "field":   <field path>,
    "nonce":   <X-E2EE-Nonce>,
    "ts":      <X-E2EE-Timestamp, integer>
  })
```

Components:

- `algo` — the algorithm string of the selected service E2EE key.
- `model` — the top-level `model` string of the request as received,
  byte-exact, with no trimming, case-folding, alias expansion, or Unicode
  normalization. Responses use the **request** model too, so the client can
  derive response AAD entirely from its own request plus clear response
  metadata. If the service later rewrites `model` for routing, AAD is
  unaffected and the rewrite is audited through the receipt. A request whose
  `model` is absent or not a string MUST be rejected with
  `e2ee_invalid_payload_model` before any AAD is built.
- `field` — the field path of the encrypted location (§7.2).
- `id` — the clear `id` string of the response object (of each chunk when
  streaming), or `""` when the response carries none.
- `nonce` / `ts` — the request's `X-E2EE-Nonce` (string) and
  `X-E2EE-Timestamp` (integer).

### 7.4 Key selection

`X-Model-Pub-Key` MUST equal one of the service's attested
`e2ee_public_keys` entries carrying a §7.1 suite; otherwise the request is
rejected with `e2ee_model_key_mismatch`. This forces the client to prove it
is encrypting to a key it could have verified.

### 7.5 Freshness and replay

- `X-E2EE-Timestamp` is Unix seconds. The service MUST reject requests where
  `|now − timestamp| > 300`, or a narrower window the service publishes
  (`e2ee_invalid_timestamp`).
- `X-E2EE-Nonce` MUST be non-empty and SHOULD carry at least 128 bits of
  randomness (for example 16 random bytes, hex-encoded).
- The service MUST reject a repeated
  `(client_public_key, service_public_key, nonce)` tuple within the
  acceptance window (`e2ee_replay_detected`). An in-memory replay cache
  spanning the window is sufficient for ACI v1.

### 7.6 Upstream encryption

Whatever encryption an aggregator speaks to its upstreams (provider-specific
handshakes, upstream E2EE) is a translation detail. It is not client-facing
ACI E2EE, is not advertised in `supported_e2ee_versions`, and appears to
clients only as channel-binding material inside receipts and attested
sessions.

## 8. Inference Receipts

A receipt is a signed, per-request event log. It binds the request bytes the
workload received, the bytes it forwarded, the upstream verification
outcome, and the response bytes it returned — all hashed inside the TEE and
signed with an attested receipt key.

### 8.1 Lookup

```text
GET /v1/aci/receipts/{id}
```

`{id}` is the `X-Receipt-Id` header value (preferred), or the
OpenAI-compatible response `id` when the response body contains one.
Receipts are retained for a bounded, implementation-defined period; clients
SHOULD fetch receipts promptly. An unknown or expired id returns
`not_found`. A receipt is finalized when the response completes: for a
streamed response it becomes available only after the stream ends (its
hashes cover the whole stream), so there is no in-flight receipt. The
`X-Receipt-Id` header is emitted with the response so the client holds the
lookup id before the receipt is queryable.

### 8.2 Receipt shape

```json
{
  "api_version": "aci/1",
  "receipt_id": "<opaque-id>",
  "chat_id": "<response-id-or-null>",
  "model": "<requested-model-or-null>",
  "workload_id": "sha256:<hex>",
  "workload_keyset_digest": "sha256:<hex>",
  "endpoint": "/v1/chat/completions",
  "method": "POST",
  "served_at": 1750000000,
  "event_log": [
    { "seq": 0, "type": "request.received",  "body_hash": "sha256:<hex>" },
    { "seq": 1, "type": "request.forwarded", "body_hash": "sha256:<hex>" },
    { "seq": 2, "type": "upstream.verified", "...": "see §8.4" },
    { "seq": 3, "type": "response.returned",
      "cleartext_hash": "sha256:<hex>", "wire_hash": "sha256:<hex>" }
  ],
  "signature": { "algo": "ecdsa-secp256k1" | "ed25519", "key_id": "<receipt-key-id>", "value": "<hex>" }
}
```

Receipts do not embed fresh attestation; they bind back to an established
`workload_id`, `workload_keyset_digest`, and receipt signing key. The two
identity fields are the receipt's self-description (§3.1):
`workload_keyset_digest` names the exact keyset that resolves
`signature.key_id`, and `workload_id` is the issuer claim. `model` is the
model the user requested (the top-level `model` of the received request,
before any service-side rewrite), so every receipt records what was asked
for even on a direct single-model service; it is `null` only when the request
carried no model. Events are flat objects: `seq` and `type` plus type-specific
fields. `seq` MUST be strictly increasing from `0`, and the first event MUST
be `request.received`.

### 8.3 Event vocabulary

All hashes are computed inside the TEE over bytes the workload actually
observed. Client-supplied hash headers are advisory at best and MUST NOT
influence receipt hashes.

| Event | Required | Fields | Meaning |
| --- | --- | --- | --- |
| `request.received` | yes, first | `body_hash` | Request body after TLS/E2EE termination and field decryption, before any mutation. |
| `request.forwarded` | yes | `body_hash` | The exact request body used for inference, after any service-side rewrite (for an aggregator, the bytes forwarded upstream). Equals `request.received.body_hash` when nothing was rewritten. |
| `response.returned` | yes | `cleartext_hash`, `wire_hash` | `wire_hash` covers the exact response body bytes emitted to the client; for a streamed (SSE) response, the in-order concatenation of the raw stream including SSE framing (`data:` lines, blank-line delimiters, any terminating sentinel) — a client verifies it by hashing the bytes it read off the wire, no reassembly needed. `cleartext_hash` covers the same body in cleartext form: identical to `wire_hash` for plaintext responses; for E2EE responses it is the service-observed stream before response-field encryption (§12). |
| `upstream.verified` | aggregator | §8.4 | Verification outcome for the upstream that served this request. |
| `response.received` | no | `cleartext_hash` | The response as first produced, before service-side transformation. |
| `transparency.request_modified` | conditional | — | MUST be present when `request.forwarded` differs from `request.received`. |
| `transparency.response_modified` | conditional | — | MUST be present when the returned bytes differ from the response as received (including E2EE re-encryption). |

Transparency events carry no fields; the hash events carry the before/after
evidence. Services MAY add further events with implementation-specific types
(the reference implementation records routing decisions, for example).
Generic verifiers MUST ignore unknown event types unless local policy
requires them. Extension events MUST NOT reuse the required event types.

### 8.4 `upstream.verified`

An aggregator receipt MUST contain an `upstream.verified` event for the
upstream that served the response (additional events for other attempts MAY
appear):

```json
{
  "seq": 2,
  "type": "upstream.verified",
  "upstream_name": "<service-chosen upstream label>",
  "provider": "<verifier adapter type or null>",
  "model_id": "<upstream model served>",
  "url_origin": "<https-origin-or-null>",
  "verifier_id": "<verifier implementation id>",
  "result": "verified" | "failed",
  "required": true | false,
  "reason": "<failure-reason-or-null>",
  "channel_bindings": [ { "...": "see below" } ],
  "provider_claims": { "...": "raw provider facts or null" },
  "session_id": "as_<hex>",
  "claims": { "...": "typed claims, §9.3" }
}
```

`session_id` and `claims` are present exactly when `result` is `"verified"`
and an attested session was sealed; `session_id` is the content-addressed
reference to it (§9). A failed verification records `reason` and no session.

Channel bindings state what the aggregator enforced when it connected to the
upstream. Defined shapes:

```json
{ "type": "tls_spki_sha256",        "origin": "<https-origin>", "spki_sha256": "<hex>" }
{ "type": "tls_certificate_sha256", "origin": "<https-origin>", "certificate_sha256": "<hex>" }
{ "type": "e2ee_public_key_sha256", "provider": "<label>", "key_id": "<optional>", "algorithm": "<algo>", "public_key_sha256": "<hex>" }
```

To a generic verifier, this event is a signed claim by the accepted
aggregator workload: it proves the attested aggregator *asserted* this
verification outcome. The deep-audit path — fetching the referenced session
and re-checking its evidence — is what upgrades the claim to independently
checked (§9, §10.3).

### 8.5 Signature

The signature covers the JCS canonicalization of the whole receipt with only
`signature.value` removed (`algo` and `key_id` stay):

```text
canonical_bytes = JCS(receipt minus signature.value)
```

- `ed25519` (RECOMMENDED) — `value` is a 64-byte RFC 8032 signature over
  `canonical_bytes`, hex-encoded. Deterministic, and verifiable with
  browser-native and standard-library cryptography.
- `ecdsa-secp256k1` — `value` is a 65-byte recoverable signature
  `r || s || v` over `sha256(canonical_bytes)`, hex-encoded. `v` is the
  recovery id (`0..3`; verifiers SHOULD also accept `27..30` minus 27). This
  is deliberately not the JOSE ES256K `r || s` shape; 64-byte signatures MUST
  be rejected. The recoverable form exists for the EVM ecosystem, where
  `ecrecover` makes on-chain receipt verification cheap.

The verifier MUST additionally check that `signature.key_id` names a key in
the established keyset's `receipt_signing_keys`, that `signature.algo`
matches that key, and that the receipt's `workload_id` and
`workload_keyset_digest` equal the established values.

### 8.6 Access control

Receipts contain hashes and verification metadata, never plaintext bodies.
When the original request carried a bearer credential, the receipt is bound
to it: retrieval MUST present the same credential (services SHOULD store
only a digest of the credential for this comparison). A missing credential
returns `unauthorized`; a non-matching one returns `redaction_required`.
Receipts for unauthenticated requests MAY be publicly retrievable.

## 9. Attested Sessions

An attested session is an immutable record of one verified upstream **TEE
channel** — the remote attested service an aggregator binds requests to. It
exists so that "the aggregator verified the upstream" is not a bare
assertion: the session carries the claims, the channel binding, and the
evidence, and its identifier is a content hash, so the record a verifier
fetches is exactly the record the receipt committed to.

Sessions are per channel, not per model or per request: a router-style
upstream that serves many models behind one TEE yields one session, and the
model served is recorded on the receipt. Re-verifying unchanged material
yields the same `session_id`; any change in the verified material (a rotated
SPKI, a new measurement, a changed claim) yields a new session.

### 9.1 Endpoints

```text
GET /v1/aci/sessions/{session_id}      one session, full evidence
GET /v1/aci/sessions?provider=&model=  list current sessions (evidence digest only)
```

Sessions carry only verification material — no request or response content —
and MAY be served without authentication as transparency artifacts. The list
endpoint is the **preflight survey**: a client can inspect the verified
identity, channel binding, and claims for a model before sending any data.
The list form omits the raw evidence `data` and keeps its digest.

### 9.2 Session record

```json
{
  "api_version": "aci/1",
  "session_id": "as_<64-hex>",
  "provider": "<service-chosen upstream label>",
  "endpoint": "<verified-upstream-origin>",
  "verifier_id": "<verifier implementation id>",
  "established_at": 1750000000,
  "expires_at": 1750003600,
  "identity": { "signing_address": "<optional>", "...": "verifier-specific keys" },
  "channel_binding": [ { "...": "same shapes as §8.4" } ],
  "claims": { "...": "§9.3" },
  "evidence": { "digest": "sha256:<hex>", "data": "data:<content-type>;base64,<...>" }
}
```

- `identity` records the verified identity keys of the upstream (for
  example a response-signing address), when the verifier established one.
- `evidence.data` is a data URI preserving the exact bytes the verifier
  consumed (a multipart bundle when there were several inputs);
  `evidence.digest` is the SHA-256 of those decoded bytes. A record whose
  `data` does not hash to `digest` MUST be rejected.
- `expires_at` is a retention deadline — at least the lifetime of receipts
  citing the session — not a validity claim. Forwarding decisions are made
  on fresh verification, not on stored sessions.

The identifier is content-addressed over the immutable material, with
timestamps and the (re-fetchable) evidence bytes excluded:

```text
material = {
  "provider":        <provider>,
  "endpoint":        <endpoint-or-null>,
  "verifier_id":     <verifier_id>,
  "identity":        <identity-or-null>,
  "channel_binding": <channel_binding array>,
  "claims":          <claims>,
  "evidence_digest": <evidence.digest-or-null>
}

session_id = "as_" || hex(sha256(JCS(material)))
```

Note the wire record omits absent optional fields (`endpoint`, `identity`,
`evidence.digest`), while the material represents them as JSON `null`; a
verifier recomputing the id restores the nulls.

Recomputing `session_id` from a fetched record is what makes it
tamper-evident; there is no separate session signature. Trust in the record
comes from the receipt: a signed receipt from an accepted workload commits
to the `session_id`, and the content addressing makes the referenced record
immutable.

### 9.3 Typed claims

Claims answer "what exactly was proven about this upstream" with a fixed
vocabulary, so that hardware-proven facts and provider marketing can never
look alike. Each claim is:

```json
{ "status": "asserted" | "refuted" | "unknown",
  "source": "hardware_proven" | "verifier_derived" | "provider_asserted" | "operator_asserted",
  "reason": "<verifier-supplied explanation>" }
```

`source` and `reason` are present only when `status` is not `unknown`.
Missing knowledge is always `unknown` — never a silent pass, and never a
refutation on an ambiguous negative.

| Claim | Meaning |
| --- | --- |
| `tee_attested` | The channel terminates in a genuine CPU TEE with the recorded identity bound to it. |
| `gpu_attested` | A confidential-computing GPU attestation was verified and nonce-bound for this channel. This attests the GPU exists and is genuine; it does not by itself prove the GPU is bound to the serving CPU TEE. |
| `tcb_up_to_date` | Platform TCB freshness as reported by the quote collateral. A stale TCB is honestly `refuted`, not hidden. |
| `os_known_good` | The platform/OS image maps to known-good provenance. |
| `serving_software_known_good` | The serving software maps to reviewed source or signed build artifacts. |
| `model_weights_provenance` | The served weights match their claimed provenance. |

An `extra` map MAY carry additional provider-scope facts verbatim (raw
verifier output such as `tcb_status`, `gpu_arch`, measurement values); these
are inputs to the typed claims, not claims themselves.

`gpu_attested` MUST NOT be asserted unless the GPU evidence is nonce-bound
to the verification round. On current hardware the GPU report has no
hardware-rooted binding to the serving CPU TEE — the gap this claim's
definition acknowledges; platforms with PCIe TDISP / TEE-I/O device binding
are expected to close it, at which point a profile can demand the stronger
statement.

The same claims object is embedded in the receipt's `upstream.verified`
event, so a **shallow audit** (trust the accepted aggregator's signature)
reads verdicts directly from the receipt, and a **deep audit** follows
`session_id` and re-checks the evidence.

## 10. Verification Procedure

Verification is adoptable in increasing depth. An SDK or integration SHOULD
state the highest level it implements:

- **Level 1 — receipt verification.** Verify receipts (§10.2) against a
  workload identity and keyset established earlier, or published by a party
  the client trusts. Fully offline once the keyset is cached.
- **Level 2 — full attestation.** Establish the identity from hardware
  evidence, key custody, and source provenance under a verifier profile
  (§10.1).
- **Level 3 — deep audit.** Additionally re-verify the aggregator's
  upstream sessions and their evidence (§10.3).

### 10.1 Establish the workload identity

Using one trusted verifier profile, check at minimum:

1. The hardware evidence verifies to the TEE vendor root.
2. `workload_id` equals the §4.1 digest of the identity public key in the
   report's keyset.
3. `workload_keyset_digest` equals the §4.2 digest of the report's keyset.
4. `report_data` equals the §4.4 statement digest for the nonce the verifier
   supplied, and the hardware evidence binds that value.
5. The keyset endorsement verifies under the identity public key (§4.3).
6. The report is fresh: the requested `nonce` is bound into `report_data`
   (step 4), `now < keyset_epoch.not_after`, and — when the profile trusts
   the platform clock — `fetched_at <= now < stale_after` (§5.1).
7. The source provenance connects the attested workload to public code or
   build artifacts acceptable to the profile.
8. Private-key custody for the listed keys satisfies the profile (§4.5).
9. `workload_identity.subject`, when present, is acceptable to the profile.
10. Any channel the client will actually use is bound: the observed TLS
    SPKI appears in `tls_public_keys` (for the hostname used, when entries
    are domain-scoped), or the E2EE key appears in `e2ee_public_keys`.

Missing evidence required by the profile is fail-closed. Only after these
checks does the client treat the workload identity as verified and release
sensitive data.

### 10.2 Verify an inference

Given an established identity and keyset, plus a response and its receipt:

1. The receipt signature verifies per §8.5 under a key listed in the
   attested `receipt_signing_keys`.
2. The receipt's `workload_id` and `workload_keyset_digest` match the
   established values.
3. `request.received.body_hash` matches the client's request bytes. For
   plaintext requests these are the bytes the client sent; for E2EE requests
   they are the decrypted body as the service observed it (§8.3).
4. `response.returned.wire_hash` matches the response bytes the client
   received — for a streamed response, the in-order concatenation of the raw
   SSE bytes read off the wire (§8.3) — and for E2EE responses
   `cleartext_hash` matches the decrypted response.
5. Transparency events are consistent: a `request.forwarded.body_hash` that
   differs from `request.received.body_hash` is accompanied by
   `transparency.request_modified`, and local policy accepts the
   modification.
6. Any extension events required by local policy are present and acceptable.

### 10.3 Audit the upstream (aggregators)

1. The receipt contains `upstream.verified` with `result: "verified"` for
   the serving upstream, with a channel binding the policy accepts (or the
   client knowingly sent `X-Upstream-Verification: none`).
2. Shallow audit: read the typed claims in the event and apply local policy
   (for example require `tee_attested` to be `asserted` with source
   `hardware_proven`).
3. Deep audit: fetch `/v1/aci/sessions/{session_id}`, recompute the
   content-addressed `session_id`, check `evidence.data` hashes to
   `evidence.digest`, and re-verify the evidence itself under the verifier
   policy for that provider.

## 11. Errors

Errors use the OpenAI-compatible shape:

```json
{ "error": { "message": "...", "type": "<type>", "code": null, "param": null } }
```

ACI-defined error types, with the HTTP status a service SHOULD use:

| Type | Status | Meaning |
| --- | --- | --- |
| `not_found` | 404 | Unknown or expired receipt / session id. |
| `unauthorized` | 401 | The receipt is credential-bound and no credential was presented. |
| `redaction_required` | 403 | The presented credential does not match the receipt owner. |
| `upstream_verification_failed` | 502 | Upstream verification was required and did not produce an enforceable verified binding; the prompt was not forwarded. |
| `e2ee_header_missing` | 400 | A required E2EE header is absent. |
| `e2ee_invalid_version` | 400 | Unsupported `X-E2EE-Version`, or the service does not terminate E2EE. |
| `e2ee_invalid_public_key` | 400 | A supplied public key does not parse. |
| `e2ee_model_key_mismatch` | 400 | `X-Model-Pub-Key` is not an attested service E2EE key. |
| `e2ee_invalid_nonce` | 400 | Empty nonce. |
| `e2ee_invalid_timestamp` | 400 | Timestamp outside the acceptance window. |
| `e2ee_replay_detected` | 400 | Repeated `(client key, service key, nonce)` tuple. |
| `e2ee_invalid_payload_model` | 400 | `model` absent or not a string (§7.3). |
| `e2ee_decryption_failed` | 400 | No field decrypted, or AAD/ciphertext mismatch. |
| `e2ee_unsupported_endpoint` | 400 | E2EE headers sent to an endpoint that does not support E2EE. |

A service MAY use a different status where an HTTP intermediary requires it
(for example 429 for rate limiting), but SHOULD preserve the `type` so
clients can branch on it. Unrecognized types are treated as opaque; clients
act on the status.

## 12. Security Considerations

- **A receipt signature is not TEE verification.** It counts only after the
  signing key is linked to an accepted `workload_id` and
  `workload_keyset_digest` through the attestation report.
- **Binding is not custody.** Every keyset entry needs a private-key custody
  story (§4.5), checked by the verifier profile.
- **Quote and endorsement are both required.** Each keyset rotation needs a
  fresh report binding the new digest; there is no endorsement-only rotation.
- **Headers are hints.** `X-ACI-Identity` and friends are unauthenticated;
  clients act on them only by re-fetching attestation.
- **Under E2EE, cleartext hashes are service-observed.** For E2EE requests,
  `request.received.body_hash` commits to the JSON body after field
  decryption as serialized by the service, not to the ciphertext the client
  sent; likewise `response.returned.cleartext_hash` for an E2EE response
  commits to the service's pre-encryption serialization. A client that wants
  to reproduce these hashes must reproduce that serialization; otherwise it
  verifies `wire_hash` (always the exact bytes it saw) and the AAD binding,
  which already ties every encrypted field to its exact request context.
- **Aggregator claims are claims.** `upstream.verified` and the session's
  typed claims are statements by the aggregator workload. Their value rests
  on (a) the aggregator's own attestation and reviewed source, and (b) the
  deep-audit path re-checking the stored evidence. The `source` field keeps
  provider assertions from masquerading as hardware proofs.
- **Receipts are for the client to keep.** A receipt is a transparency
  artifact the client retains at request time as its own record of what the
  workload committed to — not a query it runs years later. The service's
  `served_at` is self-asserted; ACI provides no trusted timestamp, so a
  receipt's meaning comes from the client correlating it, when received, to a
  response it actually got. Services retain receipts only briefly (§8.1).
- **Receipts are not a transparency log.** ACI v1 receipts prove what one
  service signed; they do not provide external timestamps or append-only
  history. Long-term non-repudiation needs an external log. Receipts and
  sessions are deliberately log-ready — signed, content-addressed, bounded —
  and IETF SCITT (RFC 9943) with COSE Receipts (RFC 9942) is the anticipated
  anchoring path.
- **ACI does not hide who is asking.** It proves what is serving and what
  happened, not client anonymity: the service sees client IPs and
  credentials. Deployments that need unlinkability compose a relay layer
  such as Oblivious HTTP (RFC 9458) in front of an ACI service; nothing in
  the protocol depends on the client's network identity.
- **ACI proves workload identity only** — not user identity, organization,
  billing, or agent delegation.

## 13. Compatibility Surfaces (informative)

Implementations MAY expose additional endpoints, headers, query parameters,
and report fields for backward compatibility with pre-ACI clients. The
reference implementation serves the inherited dstack-vllm-proxy surface:
`GET /v1/attestation/report` (a legacy report with its own report-data
layout and injected `signing_address` / `intel_quote` / `nvidia_payload`
fields), `GET /v1/signature/{id}`, and legacy E2EE modes selected by
`X-Signing-Algo`.

Compatibility surfaces MUST NOT alter ACI artifacts: canonical report and
receipt shapes, digests, and signatures are the same with or without
compatibility parameters, and legacy report bindings use separate quotes
rather than repurposing the §4.4 statement. New clients and verifiers MUST
use the `/v1/aci/*` endpoints and ignore compatibility fields.

## 14. Out of Scope for ACI v1

- Provider routing policy, upstream selection, preferences, BYOK
  credentials, billing, quotas, pricing, and canonical model ids.
- A universal verifier profile, profile registries, negotiation, or
  service-advertised profile lists.
- A public append-only transparency log for receipts or sessions (SCITT is
  the anticipated binding; see §12).
- Network metadata privacy — client IP unlinkability and anonymous
  credentials. Compose an OHTTP relay (RFC 9458) in front of an ACI service.
- Continuity across identity-key rotation (operational key rotation under
  one identity is in scope).
- Credential issuance for attestation-unaware relying parties (X.509, JWT
  issuance after verification).
- JOSE/COSE/X.509 bindings for keys and signatures (JWK key export, JWS
  receipt envelopes) — see the §4.6 design note.
- A core-defined deny-list distribution channel (CRL/OCSP equivalent) or
  transparency log for revocations. ACI defines the revocation statement and
  the identifiers a deny-list keys on (§4.7); distribution is profile- and
  deployment-specific.
- Soft rotation that changes keys without fresh attestation.
- Cross-replica key-distribution protocols.

## 15. References

Normative for the wire formats in this document:

- RFC 8785 — JSON Canonicalization Scheme (JCS).
- RFC 8032 — Ed25519 signatures.
- Intel TDX and AMD SEV-SNP attestation documentation.

Referenced for architecture and composition:

- RFC 9334 — Remote ATtestation procedureS (RATS) architecture; RFC 9711 —
  Entity Attestation Token (EAT); draft-ietf-rats-ar4si — attestation
  results vocabulary.
- RFC 9458 — Oblivious HTTP, the composable metadata-privacy layer.
- RFC 9943 / RFC 9942 — SCITT architecture and COSE Receipts, the
  anticipated transparency-log binding.
- IETF SEAT working group — attested TLS, the anticipated stronger
  transport profile.
- NVIDIA attestation suite (NRAS, nvtrust) for GPU evidence; PCIe TDISP /
  TEE-I/O for future GPU-to-TEE device binding.
- Sigstore, reproducible builds, and OpenSSF Model Signing as evidence
  formats for source and model provenance claims.
- dstack — KMS key custody and application identity model used by the
  reference implementation.
- [ACI Test Vectors](aci-test-vectors.md) — byte-exact vectors for every
  digest, canonicalization, and signature construction.
- [ACI and Related Work](aci-related-work.md) — positioning against other
  confidential-inference systems.

## Appendix A. Protocol Constants

Every identifier this version defines, in one place. A new value in any of
these sets requires a published extension document.

| Set | Values | Unknown value handling |
| --- | --- | --- |
| API version | `aci/1` (`api_version` fields, `X-ACI-Version` header) | Reject artifacts with other versions |
| Purpose strings | `aci.report_data.v1`, `aci.keyset.endorsement.v1`, `aci.keyset.revocation.v1`, `aci.e2ee.request.v2`, `aci.e2ee.response.v2` | — (fixed payload tags) |
| Signature algorithms | `ed25519` (RECOMMENDED), `ecdsa-secp256k1` | Reject |
| E2EE suites | `x25519-aes-256-gcm-hkdf-sha256` (RECOMMENDED; HKDF info `aci.e2ee.v2.x25519`), `secp256k1-aes-256-gcm-hkdf-sha256` (HKDF info `aci.e2ee.v2.secp256k1`) | Reject; other keyset entries with unknown `algo` are ignored for E2EE |
| Receipt event types | `request.received`, `request.forwarded`, `response.returned`, `response.received`, `upstream.verified`, `transparency.request_modified`, `transparency.response_modified` | Ignore; preserve for signature recomputation (§3.2) |
| Channel binding types | `tls_spki_sha256`, `tls_certificate_sha256`, `e2ee_public_key_sha256` | Treat as not enforceable |
| Claim names | `tee_attested`, `gpu_attested`, `tcb_up_to_date`, `os_known_good`, `serving_software_known_good`, `model_weights_provenance` | Extra facts live in `claims.extra`; unknown entries are informational |
| Claim statuses / sources | `asserted`, `refuted`, `unknown` / `hardware_proven`, `verifier_derived`, `provider_asserted`, `operator_asserted` | Treat the claim as `unknown` |
| TEE types | `tdx`, `sev_snp` | Requires a published verifier extension (§5.2) |
| Identifier formats | `sha256:<64-hex>` (digests), `as_<64-hex>` (session ids) | — |
| Error types | §11 table | Treat as opaque; act on HTTP status |
| Headers | §6.1, §6.2 tables | Ignore unrecognized `X-ACI-*` / `X-E2EE-*` headers |
