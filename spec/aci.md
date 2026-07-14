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
> intended: anyone may implement ACI without further permission.

Attested Confidential Inference is an interface for AI inference services
whose clients want proof, not promises. An ACI service proves **what workload
is serving the API** with hardware-rooted TEE attestation, then binds every
later artifact — TLS sessions, sealed request and response bodies,
per-request receipts, and upstream verification records — back to that
proven workload.

ACI covers OpenAI-compatible inference endpoints and adds three verification
artifacts:

| Artifact | Endpoint | Question it answers |
| --- | --- | --- |
| Attestation report | `GET /v1/aci/attestation` | What workload and which keys serve this API? |
| Inference receipt | `GET /v1/aci/receipts/{id}` | What happened for this specific request? |
| Attested session | `GET /v1/aci/sessions/{hex}` | Which verified upstream TEE served the inference (for aggregators)? |

ACI v1 does **not** define routing policy, billing, pricing, model catalogs,
canonical model identifiers, or a universal trust policy. It standardizes
bindings; each relying party chooses the verifier policy it trusts (§1.3).
For how ACI relates to other confidential-inference systems and standards,
see [ACI and Related Work](related-work.md).

## 1. Trust Model

ACI establishes two claims:

1. **Privacy.** Plaintext prompts and outputs are visible only inside
   workloads the relying party has verified and accepted.
2. **Integrity.** Responses are bound to the exact request bytes, to any
   service-side transformation, and to attested code.

A verifier accepts these claims by checking (§10.1):

- hardware-rooted TEE evidence,
- the binding of the workload keyset into that evidence,
- freshness, through the verifier's own nonce,
- source provenance, and
- private-key custody.

### 1.1 What a client must check

A channel is ACI-verifiable only when it is bound to the attested keyset:

- **TLS** — the observed server certificate's SPKI digest is listed in
  `tls_public_keys` (§4.1).
- **E2EE** — the service key is listed in `e2ee_public_keys` (§4.1).
- **Receipts** — signed by a key listed in `receipt_signing_keys` (§4.1).

A WebPKI certificate alone proves none of this, since TLS may terminate
outside the workload. A plain OpenAI SDK client gets these checks from a
verifier SDK, an agent runtime, or a local verifying proxy.

SPKI pinning is the required baseline because it works with ordinary HTTPS
stacks; attested-TLS (IETF SEAT) MAY later serve as a stronger transport
profile but does not replace it.

### 1.2 Aggregators

An aggregator is an ACI service that forwards inference to upstream
services. The aggregator is itself the client-facing workload: it proves its
own identity to clients exactly like a single-model service.

For the upstream hop, ACI v1 standardizes the aggregator's **transparency
surface**, not its routing policy:

- Before forwarding a prompt, the aggregator MUST verify the selected
  upstream and obtain an enforceable channel binding: a TLS key pin or an
  upstream E2EE key.
- If required verification fails, the aggregator MUST NOT forward the
  prompt (fail closed; §11: `upstream_verification_failed`). Service
  configuration decides which upstreams require verification; the receipt
  records the requirement and the outcome, so a client can reject
  unverified serving.
- Each receipt records the verification outcome in an `upstream.verified`
  event (§8.5).
- Each successful verification is captured as an immutable, content-addressed
  **attested session** (§9) that a verifier can fetch and re-check.

How the aggregator verifies a given upstream (which quote formats, which
measurements, which provenance) is verifier-specific and out of scope; the
recorded claims name their source (§9.3).

### 1.3 Verifier profiles

An ACI service publishes one report plus evidence; it does not negotiate
trust. Each relying party decides what convinces it: which TEE quotes it
accepts, what source provenance it requires, how it checks key custody, and
any platform-specific checks (for example dstack KMS validation). That
bundle of decisions is a **verifier profile**. A report is accepted when a
profile the relying party trusts verifies it completely.

A profile also states where each piece of evidence comes from (in the
report, fetched by digest, observed directly, or local policy) and fails
closed when required evidence is missing. Profiles can add checks but MUST
NOT relax the §10 minimum.

In RATS terms (RFC 9334): the service is the Attester, the report carries
Evidence, the relying party (or a Verifier it trusts) appraises it, and the
verifier profile is the appraisal policy; typed session claims (§9.3) play
the role of attestation results (cf. AR4SI).

### 1.4 Conformance summary

An ACI-conformant service MUST:

1. Run the client-facing workload inside a TEE with hardware-rooted
   attestation.
2. Publish its attestation report at `GET /v1/aci/attestation`, binding the
   keyset digest and the client nonce into the TEE evidence (§4.2, §5).
3. Publish source provenance connecting the attested workload to public
   code or build artifacts (§5.1).
4. Keep every listed private key in TEE custody (§4.3), and bind any
   plaintext-HTTPS endpoint's TLS key into the keyset (§4.1).
5. Support E2EE on its prompt endpoints: required on
   `POST /v1/chat/completions`, non-streaming and streaming, and
   recommended on the rest (§7).
6. Compute receipt hashes inside the TEE from observed bytes, sign receipt
   payloads with an attested key, and serve them at
   `GET /v1/aci/receipts/{id}` (§8).

An aggregator MUST additionally:

7. Verify each upstream and enforce a channel binding before forwarding a
   prompt, failing closed when required verification fails (§1.2).
8. Record the outcome in the receipt's `upstream.verified` event (§8.5)
   and publish attested sessions at `GET /v1/aci/sessions` (§9), retaining
   each session at least as long as any receipt citing it.

An ACI client (a verifier SDK, agent runtime, or verifying proxy acting for
the end user) MUST:

9. Establish the workload identity (§10.1) — itself, or through a Verifier
   it trusts — before releasing sensitive data.
10. Send sensitive data only over channels bound to the attested keyset: a
    pinned TLS SPKI or an attested E2EE key (§1.1).
11. Use fresh randomness where the protocol binds it: the attestation
    `nonce`, and a fresh ephemeral key and GCM nonce for every body it
    seals (§7.1).

An ACI verifier MUST implement at least the §10.1 checks for the profile it
applies and fail closed on missing required evidence (§1.3).

## 2. Core Terms

- **ACI service** — a service implementing this protocol.
- **Aggregator** — an ACI service that forwards inference to upstream
  services.
- **Upstream** — a service an aggregator selects to perform inference.
- **Workload keyset** — the attested document listing the workload's
  current operational public keys (receipt signing, E2EE, TLS), an optional
  `subject` name, and an expiry. The keyset is the unit of workload
  identity (§4).
- **Attestation statement** — the one-line JSON naming the keyset digest
  and the client nonce; its SHA-256 is the quote's `report_data` (§4.2).
- **Attestation report** — the service's current evidence for its keyset
  (§5).
- **Inference receipt** — a signed per-request event log (§8).
- **Attested session** — an immutable, content-addressed record of one
  verified upstream TEE channel (§9).

## 3. Artifact Conventions

Two rules cover every hash and signature in ACI:

1. **Artifacts the service builds are verified as served bytes.** The
   keyset, receipt payloads, and session documents are hashed and
   signature-checked exactly as served (after base64 transport decoding).
   A verifier consumes them without normalization.
2. **A verifier builds only two payloads itself:** the attestation
   statement (§4.2) and the E2EE AAD (§7.1). Both are fixed templates
   filled by plain string concatenation.

Conventions:

- Content ids and standalone digest fields (`workload_keyset_digest`,
  `body_hash`, `session_id`, `evidence.digest`): `sha256:<lowercase-hex>`
  over the bytes named. `report_data` and fields whose name ends in
  `_sha256` carry bare hex. Fields with the `_b64` suffix: standard base64
  (RFC 4648 §4, with padding) of the exact underlying bytes. Public keys
  and signatures: lowercase hex, no `0x` prefix.
- Services SHOULD serialize artifacts with JCS (JSON Canonicalization
  Scheme, RFC 8785): normalized output can be regenerated on demand instead
  of stored. Verifiers don't care how the bytes were made; rule 1 checks
  what was served.
- Domain separation: each verifier-constructed payload embeds its purpose —
  the `aci.report_data.v1` tag in the attestation statement, and the
  `aci.e2ee.v3.request` / `aci.e2ee.v3.response` context in the E2EE HKDF
  info and AAD. Receipt signing needs no purpose string because receipt
  keys sign nothing else (§4.1 role separation).
- Some artifacts restate a field that is derivable elsewhere, so they are
  self-describing: the report carries the keyset digest beside the keyset
  bytes, and a receipt names the digest that resolves its signing key. A
  verifier recomputes every restated field; a mismatch is a failure. **The
  attested keyset entry decides the signature algorithm, never the
  artifact.**
- [Test vectors](test-vectors.md) pin every construction byte-for-byte.

### 3.1 Extension points

- **Receipts** — services MAY add event types (§8.4); verifiers ignore
  types they don't recognize unless local policy cares. The signature
  covers the served bytes, so unknown events don't break verification. The
  top-level payload fields are fixed.
- **Session records** — the `claims.extra` map (§9.3).
- **Reports** — `attestation.evidence` is profile-defined, and consumers
  MUST ignore unrecognized `service_capabilities` members.
- **The keyset** shape is fixed; new key roles need a new protocol version.
  Every verifier implements the baseline: `ed25519` for signatures and
  `x25519-aes-256-gcm-hkdf-sha256` for E2EE. A keyset may add other
  algorithms (for example secp256k1 or P-256). The attested entry picks the
  algorithm; a verifier skips entries it can't implement (§4.1) and rejects
  an unknown algorithm instead of guessing. No negotiation, no downgrade.
- New values for other enumerated identifiers are governed by Appendix A.

## 4. Workload Identity

The **keyset is the unit of identity**; there is no separate long-lived
service keypair. The hardware quote binds the digest of the current keyset,
and every keyset change requires a fresh quote. Everything else in the
protocol chains off it:

```text
TEE hardware root of trust
      │  signs
      ▼
attestation quote ── binds ──► report_data
                                   │  sha256 of the statement naming
                                   ▼
                        workload_keyset_digest
                                   │  sha256 of
                                   ▼
                         workload keyset bytes
                                   │  lists
                                   ▼
           receipt signing keys · E2EE keys · TLS SPKIs
                                   │  verify
                                   ▼
            receipts · sealed bodies · TLS sessions
```

A verifier checks the quote once. After that, every receipt, sealed body,
and TLS connection can be checked offline against keys in the attested
keyset.

Keysets change, so recognizing the same service over time anchors on what
a workload cannot shed:

- **source provenance** — the attested code and build lineage (§5.1),
- the optional keyset **`subject`** — a profile-interpreted name attested
  with the keyset (§4.1), and
- the **domain** that serves the API.

### 4.1 Workload keyset

```json
{
  "subject": "<string-or-null>",
  "not_after": 1790000000,
  "receipt_signing_keys": [
    { "key_id": "<stable-id>", "algo": "ed25519", "public_key": "<hex>" }
  ],
  "e2ee_public_keys": [
    { "key_id": "<stable-id>", "algo": "x25519-aes-256-gcm-hkdf-sha256", "public_key": "<hex>" }
  ],
  "tls_public_keys": [
    { "spki_sha256": "<hex>", "domain": "<optional-hostname>" }
  ]
}
```

The keyset travels inside the attestation report as `workload_keyset_b64`
— the base64 of the exact keyset JSON bytes (§5.1). Its digest is over
those bytes:

```text
workload_keyset_digest = "sha256:" || hex(sha256(base64-decoded keyset bytes))
```

Rules:

- `subject` is naming metadata — a dstack app-id URI, SPIFFE ID, or DNS
  name — interpreted only by verifier profiles. Generic verifiers MUST NOT
  trust it by itself.
- `not_after` is required: a Unix timestamp after which verifiers stop
  accepting the keyset entirely (reports, TLS, E2EE, receipts). Expiry only
  helps if it is bounded, so a profile SHOULD reject an implausibly distant
  one.
- `receipt_signing_keys` hold Ed25519 keys that sign receipt payloads
  (§8.2).
- `e2ee_public_keys` MUST contain at least one key with the §7.1
  algorithm.
- `tls_public_keys` is required for services accepting sensitive plaintext
  over HTTPS. The digest is over the certificate SPKI, not the whole
  certificate, so renewals that keep the TLS key do not rotate the keyset.
  An entry MAY carry a `domain` restricting it to one public hostname; a
  client MUST pin the SPKI listed for the hostname it connects to.
- Keys are per-role: a receipt signing key MUST NOT double as an E2EE or
  TLS key.
- Entries whose `algo` is not recognized are ignored; clients select keys
  by `algo`.

Any change to the keyset — a rotated key, a changed subject, a new expiry —
produces new keyset bytes, a new digest, and a fresh attestation report
binding it. **There is no rotation path that changes keys without fresh
attestation.** A verifier that supplies a fresh nonce can never be served a
stale keyset: a quote over the old digest cannot bind the new nonce.
Historical receipts keep referencing the digest that was current when they
were signed; whether to accept an archived keyset when re-checking old
receipts is local policy.

### 4.2 Attestation binding

The hardware quote binds the current keyset and the client's freshness
challenge through one statement with exact bytes:

```text
{"keyset_digest":"sha256:<hex>","nonce":"<nonce>","purpose":"aci.report_data.v1"}
```

- No whitespace; exactly this member order.
- `sha256:<hex>` is the full `workload_keyset_digest` string.
- `<nonce>` is the value of the `nonce` query parameter of the report
  request (§5). When the parameter is absent, the `nonce` member is the
  JSON literal `null`, without quotes:

```text
{"keyset_digest":"sha256:<hex>","nonce":null,"purpose":"aci.report_data.v1"}
```

- A nonce is 1–128 characters from `[0-9A-Za-z_-]`; the service MUST
  reject anything else (HTTP 400). This is what keeps the template
  escape-free: no accepted input ever needs JSON escaping.

```text
report_data = sha256(statement bytes)
```

The 32-byte `report_data` value is placed in the TEE report-data slot
zero-padded to 64 bytes: the digest in bytes 0–31, zero in bytes 32–63.

A verifier MUST NOT accept keys that appear next to a quote but are not
bound through this calculation.

### 4.3 Key custody and replicas

Public-key binding is worthless without private-key custody. A service MUST
NOT list a public key in the keyset unless the corresponding private key is:

- generated inside the attested workload, or
- sealed exclusively to it, or
- released to it only after successful attestation of an equivalent workload
  (for example by an attestation-gated KMS).

Verifier profiles MUST specify how custody is checked for the receipt,
E2EE, and TLS keys — for example by validating a KMS signature chain
published in the report's evidence.

A deployment MAY run several replicas of the same measured workload. Each
replica holds its keys under the custody rules above and serves its own
attested keyset; ACI defines no key sharing between replicas.

### 4.4 Expiry and deny-listing

**Bounded lifetime.** Every keyset expires (`not_after`, §4.1). Expiry
limits how long stolen keys stay useful without anyone having to coordinate
a revocation: an expired keyset stops producing acceptable reports on its
own. To replace keys ahead of expiry, the service publishes a new keyset
and a fresh report (§4.1).

**Relying-party deny-list.** To reject a compromised or hostile workload
faster than expiry, relying parties deny-list on what a workload cannot
shed — its source provenance and measurements, its `subject` — or on a
specific keyset digest. List distribution (an operator endpoint, a
transparency log, an on-chain registry) is profile- and deployment-specific
(§14); archival verification under an expired or deny-listed keyset is
likewise local policy.

## 5. Attestation Report

```text
GET /v1/aci/attestation?nonce=<fresh-client-nonce>
```

Returns the service's current attestation report. The endpoint is
service-scoped: one report describes the whole workload, not one model.
Clients SHOULD supply a fresh random `nonce` (§4.2 format) and check it is
bound into `report_data`. Recency comes from the nonce; expiry comes from
the keyset's `not_after`. The report carries no other freshness metadata.

### 5.1 Response

```json
{
  "api_version": "aci/1",
  "workload_keyset_digest": "sha256:<hex>",
  "attestation": {
    "tee_type": "tdx",
    "workload_keyset_b64": "<base64 of the keyset JSON bytes, §4.1>",
    "report_data": "<hex>",
    "source_provenance": {
      "repo_url": "<https-url-or-null>",
      "repo_commit": "<git-commit-or-null>",
      "image_digest": "<sha256-prefixed-digest-or-null>",
      "image_provenance": null
    },
    "evidence": { "...": "TEE-type-specific evidence" }
  },
  "service_capabilities": {
    "supported_e2ee_versions": ["3"]
  }
}
```

The report is not signed as one object; its integrity comes from the
per-field bindings below (§3). Field rules:

- `workload_keyset_digest` MUST equal the §4.1 digest of the decoded
  `workload_keyset_b64` bytes. The top-level copy lets a relying party
  identify and cache reports before decoding; verifiers recompute it
  (§10.1).
- `report_data` MUST equal the §4.2 statement digest for the requested
  nonce, and the TEE evidence MUST bind that value.
- **Source provenance** MUST let an independent verifier connect the attested
  workload to public code or build artifacts: at least `repo_url` plus
  `repo_commit`, or `image_digest`. `image_provenance` MAY carry
  profile-interpreted build-attestation material; each provenance field is
  `null` when unknown.
  - A launcher-based profile MAY satisfy this by proving that an attested,
    provenance-checked launcher fetched and ran a pinned commit.
  - A verifier MUST reject a report without acceptable provenance (the
    wire field may be absent on non-conformant or development
    deployments).
- `service_capabilities.supported_e2ee_versions` lists the client-facing ACI
  E2EE scheme versions the service terminates (this document defines `"3"`,
  §7). A service MUST NOT advertise upstream-only encryption schemes here.

### 5.2 Evidence

`tee_type` selects the evidence format: `tdx` means Intel TDX quote
verification, `sev_snp` means AMD SEV-SNP report verification, and any other
value requires a published verifier extension. The `evidence` object is
interpreted by the verifier profile — the dstack `tdx` profile, for example,
carries the `quote`, the boot `event_log`, the booted `app_compose`, and KMS
`key_custody`, letting a verifier replay the log to the quote's RTMR3 and match
`sha256(app_compose)` to the measured `compose-hash`.

When the keyset contains domain-scoped TLS entries, the client requests
the report through a hostname the keyset lists, so the SPKI it pins is the
one for the hostname it actually uses.

## 6. Inference Endpoints

ACI v1 covers prompt endpoints: OpenAI-compatible completions and similar
formats such as Anthropic messages. Plaintext requests and responses follow
the underlying API unchanged; ACI adds headers and artifacts. E2EE requests
and responses carry the §7.2 envelope as the body.

| Endpoint | Status |
| --- | --- |
| `POST /v1/chat/completions` | REQUIRED |
| `POST /v1/completions` | OPTIONAL |
| `POST /v1/embeddings` | OPTIONAL (non-streaming only) |
| Other prompt endpoints (e.g. Anthropic-format `/v1/messages`) | OPTIONAL |
| `GET /v1/models` | OpenAI-compatible; ACI adds no required fields |

Trust metadata is service-level and lives in the attestation report. Clients
MUST NOT infer trust from `/v1/models` entries.

### 6.1 Request headers

| Header | When | Meaning |
| --- | --- | --- |
| `Authorization: Bearer <key>` | inherited | Service authentication. Also binds the receipt to this credential (§8.6). |
| `X-E2EE-Version: 3` | E2EE | E2EE scheme version; this document defines `3`. |
| `X-Client-Pub-Key` | E2EE | Client X25519 public key (hex) that the response is sealed to. |
| `X-Model-Pub-Key` | E2EE | The service E2EE public key the client selected from the attested keyset. |

### 6.2 Response headers

| Header | When | Meaning |
| --- | --- | --- |
| `X-ACI-Version: aci/1` | every response | Protocol version, including error responses. |
| `X-ACI-Keyset-Digest` | every response | The serving `workload_keyset_digest`. |
| `X-Receipt-Id` | inference responses; `upstream_verification_failed` errors (§8.5) | Lookup id for the signed receipt. |
| `X-E2EE-Applied: true \| false` | inference responses | Whether the response body is sealed. |

Headers are unauthenticated hints; what binds is always the attested
keyset and the signed receipt. On a changed `X-ACI-Keyset-Digest`, the
client SHOULD re-verify the attestation report before sending further
sensitive data.

## 7. End-to-End Encryption (E2EE)

E2EE seals whole request and response bodies between the client and the
attested workload, on top of TLS. Plaintext then reaches only a key proven
to live inside the TEE, even when TLS terminates elsewhere (load balancers,
CDNs; §1.1).

A service MUST support E2EE on `POST /v1/chat/completions` for both
non-streaming and streaming responses, and SHOULD support it on the other
prompt endpoints it serves. `X-E2EE-Version` selects the scheme;
this document defines version `3`. Versions `1` and `2` are reserved by
historical implementations and are not part of ACI.

### 7.1 Sealing

One construction seals everything, parameterized by a context string and a
recipient X25519 public key:

```text
context           = "aci.e2ee.v3.request" | "aci.e2ee.v3.response"
shared_secret     = X25519(ephemeral_private_key, recipient_public_key)
key               = HKDF-SHA256(salt = <absent>, ikm = shared_secret,
                                info = UTF-8(context), length = 32)
request_aad       = UTF-8(context) || 0x00 || UTF-8(model) || 0x00 || UTF-8(client_key_hex)
response_aad      = UTF-8(context) || 0x00 || UTF-8(model)
ciphertext || tag = AES-256-GCM(key, gcm_nonce, plaintext, aad)

sealed      = ephemeral_public_key (32) || gcm_nonce (12) || ciphertext || tag (16)
sealed_b64  = base64(sealed)
```

- `model` is the request envelope `model` (§7.2); `client_key_hex` is the
  request's `X-Client-Pub-Key` (§6.1), present only in the request AAD.
- A **sealed unit** is one request body, one buffered response body, or one
  SSE event payload. Every sealed unit MUST use a fresh ephemeral key and a
  fresh random `gcm_nonce`.
- Public keys are 32-byte X25519 keys, hex-encoded, no `0x` prefix.

### 7.2 Requests

The client sends the three E2EE headers (§6.1) and this body:

```json
{ "model": "<id>", "sealed_b64": "<base64>" }
```

- `model` MUST be a string. It and `X-Client-Pub-Key` are bound into the
  request AAD (§7.1), so a captured request cannot be replayed under another
  envelope model or resealed to a different response recipient.
- The plaintext sealed is the client's **entire original request-body
  bytes** — the exact JSON the client would have sent without E2EE, any
  modality included. The recipient key is the `X-Model-Pub-Key` service key;
  the context is `aci.e2ee.v3.request`.
- The service unseals to the client's exact original bytes and processes
  those as the request body. No re-serialization exists, so the receipt's
  `request.received` hash (§8.4) is reproducible by the client from the
  bytes it sealed.

### 7.3 Responses

Responses are sealed to `X-Client-Pub-Key` with the same envelope format,
context `aci.e2ee.v3.response`, and the same request `model` string in the
AAD. Fresh ephemeral key per sealed unit.

- **Buffered:** the response body is
  `{ "sealed_b64": "<base64>" }`, sealing the entire original response-body
  bytes.
- **Streaming:** the SSE framing stays plaintext; each event's data payload
  is replaced by `{ "sealed_b64": "<base64>" }`, sealing that event's
  original JSON bytes. The `[DONE]` sentinel stays plaintext.

### 7.4 Key selection and validation

- `X-Model-Pub-Key` MUST equal the `public_key` of an attested
  `e2ee_public_keys` entry carrying the §7.1 algorithm; otherwise the
  request is rejected with `e2ee_model_key_mismatch`. This forces the
  client to prove it is encrypting to a key it could have verified.
- A public-key header that does not parse as 32 hex-encoded bytes is
  rejected with `e2ee_invalid_public_key`.
- An `X-E2EE-Version` other than `3` is rejected with
  `e2ee_invalid_version`; a request presenting some but not all of the
  three E2EE headers is rejected with `e2ee_header_missing`.
- A body that does not parse as the §7.2 envelope, a `sealed_b64` that does
  not decode to a well-formed sealed unit, or an AEAD authentication
  failure is rejected with `e2ee_decryption_failed`.
- E2EE headers sent to an endpoint that does not support E2EE are rejected
  with `e2ee_unsupported_endpoint`.

### 7.5 Replay and response authenticity (by design)

E2EE v3 has no replay cache or timestamp window, deliberately: a replay
needs the bearer credential, and the response stays sealed to the original
client's key (bound into the request AAD, §7.1), so the residual harm is billing noise.

The envelope also carries no service signature. The sealed response is
authenticated post-hoc by the signed receipt over the wire bytes (§8.4,
§10.2), not by the envelope itself; the AEAD binds the plaintext to the
sealed bytes the receipt commits to.

### 7.6 Upstream encryption

Whatever encryption an aggregator speaks to its upstreams is a translation
detail: it is not client-facing ACI E2EE (§5.1 forbids advertising it in
`supported_e2ee_versions`) and appears to clients only as channel-binding
material inside attested sessions.

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
`X-Receipt-Id` arrives with the response, so the client holds the id before
the receipt is queryable. A receipt is finalized when the response
completes: a streamed response has no in-flight receipt (its hashes cover
the whole stream). Receipts are retained for a bounded,
implementation-defined period; clients SHOULD fetch receipts promptly. An
unknown or expired id returns `not_found`.

### 8.2 Envelope and signature

The endpoint serves a signed-bytes envelope:

```json
{
  "payload_b64": "<base64 of the receipt payload JSON bytes>",
  "key_id": "<receipt-key-id>",
  "algo": "ed25519",
  "signature": "<hex>"
}
```

`signature` is a 64-byte RFC 8032 Ed25519 signature over the decoded
`payload_b64` bytes, hex-encoded. The verifier resolves `key_id` in the
established keyset's `receipt_signing_keys`; the keyset entry decides the
algorithm, and the envelope's `algo` MUST match it (§10.2).

### 8.3 Receipt payload

```json
{
  "api_version": "aci/1",
  "receipt_id": "<opaque-id>",
  "chat_id": "<response-id-or-null>",
  "model": "<requested-model-or-null>",
  "workload_keyset_digest": "sha256:<hex>",
  "endpoint": "/v1/chat/completions",
  "method": "POST",
  "served_at": 1750000000,
  "event_log": [
    { "type": "request.received",  "body_hash": "sha256:<hex>" },
    { "type": "request.forwarded", "body_hash": "sha256:<hex>" },
    { "type": "upstream.verified", "...": "see §8.5" },
    { "type": "response.returned", "body_hash": "sha256:<hex>" }
  ]
}
```

Receipts do not embed fresh attestation; they bind back to an established
keyset through `workload_keyset_digest` and the signing key. `model` is the
model the client requested (for E2EE, the envelope `model`), `null` only
when the request carried none. Events are flat objects — `type` plus
type-specific fields — and event order is the array order. The first event
MUST be `request.received`.

### 8.4 Event vocabulary

All hashes are computed inside the TEE over bytes the workload actually
observed. Client-supplied hash headers are advisory at best and MUST NOT
influence receipt hashes.

| Event | Required | Fields | Meaning |
| --- | --- | --- | --- |
| `request.received` | yes, first | `body_hash` | The request body the workload received. Under E2EE, the hash of the **unsealed original client bytes** — reproducible by the client by construction (§7.2). Plaintext requests hash the wire body. |
| `request.forwarded` | if forwarded | `body_hash` | The exact bytes used for inference after any service-side rewrite (for an aggregator, the bytes forwarded upstream). A rewrite **is** this hash differing from `request.received`; equal hashes mean the request was untouched. Absent when the prompt was not forwarded (a §8.5 refusal). |
| `upstream.verified` | aggregator | §8.5 | Verification outcome for the upstream that served this request. |
| `response.returned` | yes | `body_hash` | The exact response body bytes emitted on the wire — for a §8.5 refusal, the error body served in place of an inference response. For SSE, the raw in-order stream including framing (`data:` lines, delimiters, terminating sentinel). For E2EE, the sealed envelope bytes — the plaintext binding comes from the AEAD (§7.5). |

Services MAY add events with implementation-specific types (the reference
implementation records routing decisions, for example), but MUST NOT reuse
the required types. Verifiers ignore event types they don't recognize
unless local policy cares.

### 8.5 `upstream.verified`

An aggregator receipt MUST contain an `upstream.verified` event for the
upstream that served the response (additional events for other attempts MAY
appear). Its two forms:

```json
{ "type": "upstream.verified", "result": "verified",
  "required": true, "model_id": "<upstream model served>",
  "session_id": "sha256:<hex>" }

{ "type": "upstream.verified", "result": "failed",
  "required": true, "model_id": "<upstream model requested>",
  "reason": "<failure reason>", "upstream_name": "<optional label>" }
```

- `required` records whether service configuration required verification
  for this upstream. When `required` is `true` and `result` is `"failed"`,
  the prompt was not forwarded (§1.2): such a receipt accompanies an
  `upstream_verification_failed` error, not an inference response, and the
  error response carries `X-Receipt-Id` (§6.2) so the client can fetch it.
- A verified event carries `session_id` — the content address of the
  attested session (§9) holding every other verification detail: upstream
  name, endpoint, verifier id, channel bindings, typed claims, raw
  provider facts, and evidence. The detail is deduplicated: thousands of
  receipts point at one session.
- A failed event carries `reason` instead, and MAY carry `upstream_name`;
  no session is created.

To a generic verifier this event proves only that the attested aggregator
*asserted* the outcome; deep audit (§10.3) upgrades it to independently
checked.

### 8.6 Access control

Receipts contain hashes and verification metadata, never plaintext bodies.
When the original request carried a bearer credential, the receipt is bound
to it: the service MUST require the same credential to serve the receipt,
and SHOULD store only a digest of the credential for that comparison. A missing credential
returns `unauthorized`; a non-matching one returns `redaction_required`
(the receipt exists but is withheld). Receipts for unauthenticated requests
MAY be publicly retrievable.

## 9. Attested Sessions

An attested session is an immutable record of one verified upstream **TEE
channel** — the remote attested service an aggregator binds requests to —
for one validity period. The served bytes are the artifact:

```text
session_id = "sha256:" || hex(sha256(exact served session document bytes))
```

The id is not inside the document. The signed receipt commits to
`session_id`, so recomputing the hash of the fetched bytes proves the
record is exactly what the receipt cited; there is no session signature.

Sessions are per channel and per validity period, not per model or per
request: a router-style upstream serving many models behind one TEE yields
one session, and the model served is recorded on each receipt.
Re-verification — after `expires_at`, or whenever the verified material
changes — produces a new session document with a new period and a new id;
sessions are never updated in place.

**Retention.** A session MUST stay retrievable, byte-identical, for as
long as any receipt cites it. `expires_at` ends the validity period for
new forwarding decisions, not the retention obligation.

### 9.1 Endpoints

```text
GET /v1/aci/sessions/{hex}                  one session, full evidence
GET /v1/aci/sessions?upstream_name=&model=  list current sessions
```

`{hex}` is the session id's 64-hex digest (the id without the `sha256:`
prefix). Sessions carry only verification material — no request or response
content — and MAY be served without authentication as transparency
artifacts.

The list endpoint is a convenience: a client can inspect the verified
identity, channel binding, and claims for a model before sending any data.
List entries add a `session_id` member for lookup and omit the raw
`evidence.data`, keeping `evidence.digest`. Only the full record's served
bytes hash to the session id.

### 9.2 Session record

```json
{
  "api_version": "aci/1",
  "upstream_name": "<service-chosen upstream label>",
  "endpoint": "<verified-upstream-origin-or-null>",
  "verifier_id": "<verifier implementation id>",
  "established_at": 1750000000,
  "expires_at": 1750003600,
  "identity": { "signing_address": "<optional>", "...": "verifier-specific keys" },
  "channel_binding": [ { "...": "shapes below" } ],
  "claims": { "...": "§9.3" },
  "evidence": { "digest": "sha256:<hex>", "data": "data:<content-type>;base64,<...>" }
}
```

- `identity` records the verified identity keys of the upstream (for
  example a response-signing address), when the verifier established one;
  its members are verifier-specific.
- `evidence.data` is a data URI preserving the exact bytes the verifier
  consumed (a multipart bundle when there were several inputs);
  `evidence.digest` is the SHA-256 of those decoded bytes. A verifier MUST
  reject a record whose `data` does not hash to `digest`.

`channel_binding` states what the aggregator enforced when it connected to
the upstream. Defined shapes:

```json
{ "type": "tls_spki_sha256",        "origin": "<https-origin>", "spki_sha256": "<hex>" }
{ "type": "tls_certificate_sha256", "origin": "<https-origin>", "certificate_sha256": "<hex>" }
{ "type": "e2ee_public_key_sha256", "provider": "<label>", "key_id": "<optional>", "algorithm": "<algo>", "public_key_sha256": "<hex>" }
```

### 9.3 Typed claims

Claims answer "what exactly was proven about this upstream" with a fixed
vocabulary, so that hardware-proven facts and provider marketing can never
look alike. Each claim is:

```text
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
verifier output such as `tcb_status`, `gpu_arch`, measurement values);
these are inputs to the typed claims, not claims themselves. The key names
inside `extra` are a stable contract for a given verifier: consumers may
depend on them, and a verifier MUST NOT rename or repurpose a published
key.

A verifier MUST NOT assert `gpu_attested` unless the GPU evidence is
nonce-bound to the verification round. PCIe TDISP / TEE-I/O is expected to
close the CPU-binding gap noted above, at which point a profile can demand
the stronger statement.

Receipts do not embed claims; they cite the session that carries them
(§8.5). §10.3 defines the shallow and deep audits over it.

## 10. Verification Procedure

Verification comes in three levels. An SDK or integration SHOULD state the
highest level it implements:

- **Level 1 — receipt verification.** Verify receipts (§10.2) against a
  keyset established earlier, or published by a party the client trusts.
  Fully offline once the keyset is cached.
- **Level 2 — full attestation.** Establish the workload identity from
  hardware evidence, key custody, and source provenance under a verifier
  profile (§10.1).
- **Level 3 — deep audit.** Additionally re-verify the aggregator's
  upstream sessions and their evidence (§10.3).

### 10.1 Establish the workload identity

Using one trusted verifier profile, check at minimum:

1. **Hardware.** The TEE evidence verifies to the vendor root and binds
   `report_data` (32 bytes, zero-padded to 64, in the report-data slot;
   §4.2).
2. **Binding and freshness.** Recompute the chain: base64-decode
   `workload_keyset_b64`; the SHA-256 of the decoded bytes equals
   `workload_keyset_digest`; build the §4.2 statement from that digest and
   the nonce you supplied; the SHA-256 of the statement equals
   `report_data`. One recomputation establishes that the keyset is exactly
   what the quote bound and that the quote postdates your challenge.
3. **Expiry.** `now < not_after` in the decoded keyset.
4. **Provenance.** The source provenance connects the attested workload to
   public code or build artifacts acceptable to the profile (§5.1).
5. **Custody.** Private-key custody for the listed keys satisfies the
   profile (§4.3), and `subject`, when present, is acceptable to it.
6. **Channel.** The channel actually used is bound: the observed TLS SPKI
   is listed in `tls_public_keys` (for the hostname used, when entries are
   domain-scoped), or the E2EE key used is listed in `e2ee_public_keys`.

Missing evidence required by the profile is fail-closed. Only after these
checks does the client treat the workload as verified and release sensitive
data.

### 10.2 Verify an inference

Given an established keyset, plus a response and its receipt envelope:

1. **Signature.** `signature` is a valid Ed25519 signature over the decoded
   `payload_b64` bytes under the key that `key_id` names in the established
   keyset's `receipt_signing_keys`; the envelope's `algo` matches that
   keyset entry.
2. **Keyset.** The payload's `workload_keyset_digest` equals the
   established digest.
3. **Request.** `request.received.body_hash` matches the client's request
   bytes — the wire body for plaintext requests, the original body the
   client sealed for E2EE requests (§8.4).
4. **Response.** `response.returned.body_hash` matches the response bytes
   the client received off the wire — the in-order raw SSE bytes for a
   stream, the sealed envelope bytes for E2EE (whose plaintext the client
   already authenticated through the AEAD, §7.5).

To see service-side rewrites, compare `request.forwarded.body_hash` with
`request.received.body_hash`: differing hashes are the rewrite. Whether a
rewrite is acceptable is local policy; ACI records it, nothing more.

### 10.3 Audit the upstream (aggregators)

1. The receipt's `upstream.verified` event has `result: "verified"` and
   cites a `session_id`. A client that requires verified upstreams rejects
   receipts where `result` is `"failed"` or `required` is `false`.
2. Fetch `/v1/aci/sessions/{hex}`; recompute the session id from the
   fetched bytes (§9) and check it equals the cited `session_id`.
3. The receipt's `served_at` falls within the session's `established_at`
   to `expires_at` window. `served_at` is self-asserted (§12), so this
   catches an honest service citing an expired session; against a
   dishonest one, the fail-closed rule (§1.2) rests on the attested code.
4. `evidence.data` decodes and hashes to `evidence.digest`.
5. Shallow audit: apply local policy to the session's channel bindings and
   typed claims (for example, require `tee_attested` to be `asserted` with
   source `hardware_proven`).
6. Deep audit: re-verify the evidence itself under the relying party's
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
| `e2ee_header_missing` | 400 | Some but not all required E2EE headers are present. |
| `e2ee_invalid_version` | 400 | Unsupported `X-E2EE-Version`, or the service does not terminate E2EE. |
| `e2ee_invalid_public_key` | 400 | A supplied public key does not parse as 32 hex-encoded bytes. |
| `e2ee_model_key_mismatch` | 400 | `X-Model-Pub-Key` is not an attested service E2EE key. |
| `e2ee_decryption_failed` | 400 | The envelope does not parse, the sealed unit is malformed, or AEAD authentication fails (§7.4). |
| `e2ee_unsupported_endpoint` | 400 | E2EE headers sent to an endpoint that does not support E2EE. |

A service MAY use a different status where an HTTP intermediary requires it
(for example 429 for rate limiting), but SHOULD preserve the `type` so
clients can branch on it. Unrecognized types are treated as opaque; clients
act on the status.

## 12. Security Considerations

- **A receipt signature is not TEE verification.** It counts only after the
  signing key is linked to an accepted `workload_keyset_digest` through the
  attestation report.
- **Binding is not custody.** Every keyset entry needs a private-key custody
  story (§4.3), checked by the verifier profile.
- **Headers are hints** (§6.2): unauthenticated; act on a change only by
  re-fetching attestation.
- **`gpu_attested` does not bind the GPU to the CPU TEE.** It proves a
  genuine, nonce-bound confidential-computing GPU exists (§9.3), not that
  it is the GPU wired to the serving CPU TEE; TDISP / TEE-I/O is expected
  to close this gap.
- **Sealed responses are authenticated after the fact:** the E2EE envelope
  carries no service signature; the signed receipt over the wire bytes,
  tied to the plaintext by the AEAD, attributes a sealed response (§7.5).
- **Sealed-request replay is tolerated by design:** a replay needs the
  bearer credential, and the response is sealed to the original client's
  key, so the residual harm is billing noise (§7.5).
- **Aggregator claims are claims** — statements by the aggregator workload,
  worth what its own attestation plus deep audit (§10.3) make them; `source`
  keeps provider assertions distinct from hardware proofs.
- **Receipts are records for the client, not a transparency log.** The
  client fetches its receipt promptly (§8.1) and correlates it to a response
  it actually got; `served_at` is self-asserted, and ACI provides no trusted
  timestamp or append-only history. Long-term non-repudiation needs an
  external log — receipts and sessions are log-ready (signed,
  content-addressed, bounded), with SCITT (RFC 9943) and COSE Receipts
  (RFC 9942) the anticipated anchor.
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
fields), `GET /v1/signature/{id}`, and the no-AAD legacy E2EE mode selected
by `X-Signing-Algo`.

Compatibility surfaces MUST NOT alter ACI artifacts: report, receipt, and
session bytes, digests, and signatures are the same with or without
compatibility parameters. Legacy report bindings use separate quotes rather
than repurposing the §4.2 statement. New clients and verifiers use the
`/v1/aci/*` endpoints and ignore compatibility fields.

## 14. Out of Scope for ACI v1

- Provider routing policy, upstream selection, preferences, BYOK
  credentials, billing, quotas, pricing, and canonical model ids.
- A universal verifier profile, profile registries, negotiation, or
  service-advertised profile lists.
- A public append-only transparency log for receipts or sessions (SCITT is
  the anticipated binding; see §12).
- Network metadata privacy — client IP unlinkability and anonymous
  credentials (compose an OHTTP relay, §12).
- A stable service identity that survives keyset changes: relying parties
  anchor on `subject`, source provenance, and the domain (§4).
- Credential issuance for attestation-unaware relying parties (X.509, JWT
  issuance after verification).
- JOSE/COSE/X.509 bindings for keys, receipts, and sessions.
- A core-defined deny-list distribution channel (CRL/OCSP equivalent); ACI
  names what to deny — provenance, measurements, `subject`, keyset digests
  (§4.4) — not how the list is distributed.
- Key rotation without fresh attestation.

## 15. References

Normative for the wire formats in this document:

- RFC 8032 — Ed25519 signatures.
- RFC 7748 — X25519 key agreement.
- RFC 5869 — HKDF.
- RFC 4648 — base64 encoding.
- Intel TDX and AMD SEV-SNP attestation documentation.

RFC 8785 (JSON Canonicalization Scheme) helps producers emit deterministic
bytes. Verifiers don't use it; they check the bytes as served (§3).

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
- [ACI Test Vectors](test-vectors.md) — byte-exact vectors for every
  digest and signature construction.
- [ACI and Related Work](related-work.md) — positioning against other
  confidential-inference systems.

## Appendix A. Protocol Constants

Every identifier this version defines, in one place. A new value in any of
these sets requires a published extension document.

| Set | Values | Unknown value handling |
| --- | --- | --- |
| API version | `aci/1` (`api_version` fields, `X-ACI-Version` header) | Reject artifacts with other versions |
| Purpose / context strings | `aci.report_data.v1`, `aci.e2ee.v3.request`, `aci.e2ee.v3.response` | — (fixed payload tags) |
| Signature algorithms | `ed25519` baseline; keysets may carry more (§3.1) | Ignore a keyset entry whose `algo` is unknown; reject an artifact signed with one |
| E2EE algorithms | `x25519-aes-256-gcm-hkdf-sha256` baseline; keysets may carry more (§3.1) | Ignore a keyset entry whose `algo` is unknown; reject a request that selects one |
| E2EE versions | `3` (`1` and `2` reserved-historical) | Reject (`e2ee_invalid_version`) |
| Receipt event types | `request.received`, `request.forwarded`, `response.returned`, `upstream.verified` | Ignore (§8.4) |
| Channel binding types | `tls_spki_sha256`, `tls_certificate_sha256`, `e2ee_public_key_sha256` | Treat as not enforceable |
| Claim names | `tee_attested`, `gpu_attested`, `tcb_up_to_date`, `os_known_good`, `serving_software_known_good`, `model_weights_provenance` | Extra facts live in `claims.extra`; unknown entries are informational |
| Claim statuses / sources | `asserted`, `refuted`, `unknown` / `hardware_proven`, `verifier_derived`, `provider_asserted`, `operator_asserted` | Treat the claim as `unknown` |
| TEE types | `tdx`, `sev_snp` | Requires a published verifier extension (§5.2) |
| Identifier format | `sha256:<64-hex>` for content ids and standalone digests (`workload_keyset_digest`, `body_hash`, `session_id`, `evidence.digest`); bare hex for `report_data` and `*_sha256` fields (§3) | — |
| Error types | §11 table | Treat as opaque; act on HTTP status |
| Headers | §6.1, §6.2 tables | Ignore unrecognized `X-ACI-*` / `X-E2EE-*` headers |
