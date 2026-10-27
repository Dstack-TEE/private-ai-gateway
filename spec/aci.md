# Attested Confidential Inference (ACI) Specification

> **Version:** `aci/1` (draft)
> **Audience:** security researchers evaluating the protocol, and inference
> providers or aggregators implementing it.
> **Conformance language:** MUST, SHOULD, and MAY are used in the RFC 2119
> sense.
> **License:** Apache License 2.0 (see `LICENSE`). The patent grant is
> intended: anyone may implement ACI without further permission.

Attested Confidential Inference (ACI) lets an AI inference service prove
**what workload is serving its API**, with hardware-rooted TEE
attestation. Every later artifact binds back to that proven workload: TLS
sessions, optional E2EE extension channels, per-request receipts, and upstream
verification records.

ACI covers OpenAI-compatible inference endpoints and adds three verification
artifacts:

| Artifact | Endpoint | Question it answers |
| --- | --- | --- |
| Attestation report | `GET /v1/aci/attestation` | What workload and which keys serve this API? |
| Inference receipt | `GET /v1/aci/receipts/{id}` | What happened for this specific request? |
| Attested session | `GET /v1/aci/sessions/{session_id}` | Which verified upstream TEE served the inference (for aggregators)? |

ACI v1 does **not** define routing policy, billing, pricing, model catalogs,
canonical model identifiers, or a universal trust policy — nor profile
registries, credential issuance (X.509/JWT), or JOSE/COSE bindings. It
proves workload identity, not user identity or delegation. It standardizes
bindings; each relying party verifies under its own policy (§1.3).
For how ACI relates to other confidential-inference systems and standards,
see [ACI and Related Work](related-work.md).

## 1. Trust Model

ACI establishes two claims:

1. **Privacy.** Plaintext prompts and outputs are visible only inside
   workloads the relying party has verified and accepted.
2. **Integrity.** Responses are bound to the exact request bytes, to any
   service-side transformation, and to attested code.

A verifier accepts these claims by checking (§9.1):

- hardware-rooted TEE evidence,
- the binding of the workload keyset into that evidence,
- freshness, through the verifier's own nonce,
- source provenance, and
- private-key custody.

### 1.1 What a client must check

A channel is ACI-verifiable only when it is bound to the attested keyset:

- **TLS** — the observed server certificate's SPKI digest is listed in
  `tls_public_keys` (§3.1).
- **E2EE** — the service key is listed in `e2ee_public_keys` (§3.1).
- **Receipts** — signed by a key listed in `receipt_signing_keys` (§3.1).

A WebPKI certificate alone proves none of this, since TLS may terminate
outside the workload. A plain OpenAI SDK client gets these checks from a
verifier SDK or proxy.

SPKI pinning is the required baseline because it works with ordinary
HTTPS stacks. Attested-TLS (IETF SEAT) MAY later serve as a stronger
transport binding but does not replace it.

### 1.2 Aggregators

An aggregator is an ACI service that forwards inference to upstream
services. The aggregator is itself the client-facing workload: it proves its
own identity to clients exactly like a single-model service.

For the upstream hop, ACI v1 standardizes the aggregator's **transparency
surface**, not its routing policy:

- Every upstream that offers TEE attestation is verified before it serves,
  and the aggregator reaches it only over the channel that verification
  bound — a TLS key pin, an upstream E2EE key, or a binding a published
  extension defines (Appendix B). Each receipt records the
  outcome (§7.5). Nothing a client sends skips this.
- Verified serving is required when the operator configures the serving
  endpoint TEE-only, or when the request carries `aci_verified` (§5.3).
  Then non-TEE upstreams are not candidates, and a failed or unavailable
  verification refuses the prompt (fail closed, §10:
  `upstream_verification_failed`).
- Otherwise a service MAY also route to upstreams with no TEE (an ordinary
  commercial API), so a client can deliberately choose one. The receipt
  records the serving as unverified (`required: false`, §7.5) and no
  attested session exists (§8).
- A successful verification establishes an immutable, content-addressed
  attested session (§8), or revalidates the verified channel material
  represented by an unexpired session. The session preserves the evidence used
  to establish it.

How the aggregator verifies a given upstream (which quote formats, which
measurements, which provenance) is verifier-specific and out of scope.
The recorded claims name their source (§8.3).

### 1.3 Verifier policies

An ACI service publishes one report plus evidence, the same for every
client. Each relying party decides for itself whether that evidence is
enough. Those decisions are its **verifier policy**: the TEE roots it
trusts, the source provenance it requires, how it checks key custody, and
any platform-specific checks such as dstack KMS validation.

A verifier runs the §9 checks under one policy. A policy may require
more than §9, never less. When evidence the policy needs is missing,
verification fails.

In RATS terms (RFC 9334): the service is the Attester, the report carries
Evidence, and the relying party (or a Verifier it trusts) appraises it
under the policy. Typed session claims (§8.3) are the attestation results
(cf. AR4SI).

### 1.4 Conformance summary

An ACI-conformant service MUST:

1. Run the client-facing workload inside a TEE with hardware-rooted
   attestation.
2. Publish its attestation report at `GET /v1/aci/attestation`, binding the
   keyset digest and the client nonce into the TEE evidence (§3.2, §4).
3. Publish source provenance connecting the attested workload to public
   code or build artifacts (§4.1).
4. Keep every listed private key in TEE custody (§3.3), and bind any
   plaintext-HTTPS endpoint's TLS key into the keyset (§3.1).
5. Compute receipt hashes inside the TEE from observed bytes, sign
   receipts with an attested key, and serve them at
   `GET /v1/aci/receipts/{id}` (§7).

An aggregator MUST additionally:

6. Verify upstreams and enforce channel bindings as §1.2 requires,
   failing closed when required verification fails or the client's
   serving constraints cannot be met (§5.3).
7. Cite the attested session in each receipt served through a verified
   upstream, and serve those sessions with their evidence (§7.5, §8).

An ACI client (a verifier SDK, agent runtime, or verifying proxy acting for
the end user) MUST:

8. Verify the service's attested code, environment, and keyset (§9.1),
   on its own or through a Verifier it trusts, before releasing sensitive
   data.
9. Send sensitive data only over channels bound to the attested keyset: a
   pinned TLS SPKI or an attested E2EE key used according to a separately
   specified extension (§1.1, §6).
10. Use a fresh attestation `nonce` wherever freshness is required (§3.2).

An ACI verifier MUST implement at least the §9.1 checks for the policy it
applies and fail closed on missing required evidence (§1.3).

## 2. Core Terms

- **ACI service** — a service implementing this protocol.
- **Aggregator** — an ACI service that forwards inference to upstream
  services.
- **Upstream** — a service an aggregator selects to perform inference.
- **Workload keyset** — the workload's cryptographic identity: an attested
  document listing its current public keys (receipt signing, E2EE, TLS),
  an optional `subject` name, and an expiry (§3).
- **Attestation statement** — the one-line JSON naming the keyset digest
  and the client nonce. Its SHA-256 is the quote's `report_data` (§3.2).
- **Attestation report** — the service's current evidence for its keyset
  (§4).
- **Inference receipt** — a signed per-request event log (§7).
- **Attested session** — an immutable, content-addressed record of one
  verified upstream TEE channel (§8).

Byte-level conventions (encodings, serialization, domain separation)
live in Appendix A.

## 3. Workload Identity

A workload's identity is cryptographic: the **keyset is the identity**,
and there is no separate long-lived service keypair. The hardware quote
binds the digest of the current keyset, and every keyset change requires a
fresh quote. Everything else in the protocol chains off it:

```text
TEE hardware root of trust
      │  signs
      ▼
attestation quote
      │  commits to (§3.2)
      ▼
attestation statement   (keyset digest + client nonce)
      │  pins (§3.1)
      ▼
workload keyset         (receipt · E2EE · TLS public keys)
      │  verifies
      ▼
receipts · E2EE fields · TLS sessions
```

Two SHA-256 links join the chain: `report_data` in the quote is the hash
of the statement (§3.2), and the statement names the hash of the keyset
(§3.1).

A verifier checks the quote once. After that, every receipt, E2EE field,
and TLS connection can be checked against keys in the attested keyset.

Keysets change. To recognize the same service over time, rely on what a
workload cannot shed:

- **source provenance** — the attested code and build lineage (§4.1),
- the optional keyset **`subject`** — a policy-interpreted name attested
  with the keyset (§3.1), and
- the **domain** that serves the API.

### 3.1 Workload keyset

```json
{
  "subject": "<string-or-null>",
  "not_after": 1790000000,
  "receipt_signing_keys": [
    { "key_id": "<stable-id>", "algo": "ed25519", "public_key": "<hex>" }
  ],
  "e2ee_public_keys": [
    { "key_id": "<stable-id>", "algo": "<extension-defined-algorithm>", "public_key": "<extension-defined-encoding>" }
  ],
  "tls_public_keys": [
    { "spki_sha256": "<hex>", "domain": "<optional-hostname>" }
  ]
}
```

The digest is over the keyset's JCS form (Appendix A):

```text
workload_keyset_digest = "sha256:" || hex(sha256(JCS(workload_keyset)))
```

The report embeds the keyset as a plain JSON object (§4.1). The served
encoding is free: a verifier canonicalizes the keyset it parsed and
hashes that.

Rules:

- `subject` is naming metadata — a dstack app-id URI, SPIFFE ID, or DNS
  name — meaningful only under a verifier policy. Generic verifiers MUST
  NOT trust it by itself.
- `not_after` is required: a Unix timestamp after which verifiers MUST NOT
  establish the keyset from a new report or use it to authorize new TLS,
  E2EE, or inference sends. A previously established keyset MAY still verify
  a historical receipt whose `served_at` is earlier than that keyset's
  `not_after`, subject to all of §9.3. A verifier SHOULD reject an implausibly
  distant `not_after`.
- `receipt_signing_keys` hold the keys that sign receipts (§7.2) —
  `ed25519` baseline (Appendix B).
- `e2ee_public_keys` contains keys for separately specified E2EE extensions
  (§6). It MAY be empty when the service advertises no E2EE versions. An
  advertised extension defines its accepted algorithms and required keys.
- `tls_public_keys` is required for services accepting sensitive plaintext
  over HTTPS. The digest is over the certificate SPKI, not the whole
  certificate, so renewals that keep the TLS key do not rotate the keyset.
  An entry MAY carry a `domain` restricting it to one public hostname.
  A client MUST pin the SPKI listed for the hostname it connects to.
- Keys are per-role: a receipt signing key MUST NOT double as an E2EE or
  TLS key.
- Entries whose `algo` is not recognized are ignored. Clients select
  keys by `algo`.

Changing any key or identity field produces a new identity and requires a
fresh attestation report. A **renewal** changes only `not_after` within the
family defined below. It has a new digest; a quote over the old digest cannot
bind a fresh nonce.

Within one API origin and an unchanged verifier policy, two verified reports
belong to the same **renewal family** exactly when:

- Their complete `workload_keyset` objects, including extension fields, have
  identical JCS forms after removing only `not_after`. This includes `subject`
  and every entry, algorithm, identifier, key value, and domain in all key
  roles. Array order remains significant.
- Their `attestation.tee_type` values are identical.
- Their complete protocol-defined `source_provenance` structs are equal:
  `repo_url`, `repo_commit`, `image_digest`, and `image_provenance`, with
  missing fields normalized to null and structured values compared by JCS.
  Unknown provenance extension fields are outside this struct.
- Their policy-verified measured workload is identical. For the dstack policy
  (§4.2), this is the same policy-verified measured `sha256(app_compose)`.
  Fresh quote bytes, nonce bindings, and other changing verification evidence
  are not family identifiers; each report still MUST pass §9.1.

Replicas can expose members of one renewal family with different expiry times.
A fresh verification of any same-family keyset keeps continuity while that
keyset is unexpired, even when its `not_after` is earlier than the previous
member's. A digest header is only a re-verification hint: fresh verification
of any unexpired same-family member authorizes future sends, even when it
reaches a different replica. A changed family revokes the old delivery gate.
A receipt still requires its exact digest to have been established (§9.3);
failure to resolve that digest fails its audit without blocking sends under
the freshly verified family. This rule provides no monotonic version or
rollback protection.

### 3.2 Attestation binding

The hardware quote binds the current keyset and the client's freshness
challenge through one statement with exact bytes:

```text
{"keyset_digest":"sha256:<hex>","nonce":"<nonce>","purpose":"aci.report_data.v1"}
```

- No whitespace and exactly this field order — the template is its own
  JCS form (Appendix A).
- `sha256:<hex>` is the full `workload_keyset_digest` string.
- `<nonce>` is the value of the `nonce` query parameter of the report
  request (§4). When the parameter is absent, the `nonce` field is the
  JSON literal `null`, without quotes — the unchallenged form, cacheable
  but proving no freshness (§9.1):

```text
{"keyset_digest":"sha256:<hex>","nonce":null,"purpose":"aci.report_data.v1"}
```

- A nonce is a 32-byte value sent as exactly 64 lowercase hex
  characters. The service MUST reject anything else (HTTP 400, error
  type `invalid_request_error`). Every statement input is hex or a fixed tag,
  so the template never needs JSON escaping.

```text
report_data = sha256(statement bytes)
```

The 32-byte `report_data` value is placed in the TEE report-data slot
zero-padded to 64 bytes: the digest in bytes 0–31, zero in bytes 32–63.

A verifier MUST NOT accept keys that appear next to a quote but are not
bound through this calculation.

### 3.3 Key custody and replicas

A service MUST NOT list a public key in the keyset unless the
corresponding private key is:

- generated inside the attested workload, or
- sealed exclusively to it, or
- released to it only after successful attestation of an equivalent workload
  (for example by an attestation-gated KMS).

Verifier policies MUST specify how custody is checked for the receipt,
E2EE, and TLS keys — for example by validating a KMS signature chain
published in the report's evidence.

A deployment MAY run several replicas of the same measured workload. Each
replica holds its keys under the custody rules above and serves its own
attested keyset. ACI defines no key sharing between replicas.

### 3.4 Expiry and deny-listing

**Bounded lifetime.** Every keyset expires (`not_after`, §3.1). A service may
renew a keyset before expiry by publishing the same keys and identity with a
later `not_after` and a fresh report. A real key rotation publishes a new
keyset and report.

**Relying-party deny-list.** To drop a compromised workload faster than
expiry, a relying party deny-lists its stable identifiers (provenance,
measurements, or `subject`) or one specific keyset digest. How
the list reaches verifiers (an operator endpoint, a transparency log, an
on-chain registry) is up to the deployment. Whether old receipts still
verify under a deny-listed keyset is each relying party's own call.

## 4. Attestation Report

```text
GET /v1/aci/attestation?nonce=<fresh-client-nonce>
```

Returns the service's current attestation report. The endpoint is
service-scoped: one report describes the whole workload, not one model.
Clients SHOULD supply a fresh random 32-byte `nonce` (64 hex characters,
§3.2) and check it is bound into `report_data`. Recency comes from the
nonce. Expiry comes from the keyset's `not_after`. The report carries no
other freshness metadata.

### 4.1 Response

```json
{
  "api_version": "aci/1",
  "workload_keyset_digest": "sha256:<hex>",
  "attestation": {
    "tee_type": "tdx",
    "workload_keyset": { "...": "the keyset object, §3.1" },
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
    "supported_e2ee_versions": ["2"],
    "supported_multipart_constraint_endpoints": ["/v1/audio/transcriptions"],
    "serving": "direct"
  }
}
```

The report is not signed as one object. Its integrity comes from the
per-field bindings below (Appendix A). Field rules:

- `workload_keyset_digest` MUST equal the §3.1 digest of the embedded
  `workload_keyset`. The top-level copy lets a relying party identify and
  cache reports cheaply. Verifiers recompute it (§9.1).
- `report_data` MUST equal the §3.2 statement digest for the requested
  nonce, and the TEE evidence MUST bind that value.
- **Source provenance** MUST let an independent verifier connect the attested
  workload to public code or build artifacts: at least `repo_url` plus
  `repo_commit`, or `image_digest`. `image_provenance` MAY carry
  policy-interpreted build-attestation material. Each provenance field
  is `null` when unknown.
  - These fields are not bound into the quote, so a verifier trusts them
    only when corroborated by measured evidence, like the §4.2
    compose-hash path.
  - A launcher-based policy MAY satisfy this by proving that an attested,
    provenance-checked launcher fetched and ran a pinned commit.
  - A verifier MUST reject a report without acceptable provenance (the
    field may be absent on development deployments).
- `service_capabilities.supported_e2ee_versions` lists separately specified,
  client-facing E2EE extension versions the service terminates (§6). A service
  MUST NOT advertise upstream-only encryption schemes here.
- `service_capabilities.serving` is `"direct"` when inference runs inside
  this attested workload and `"aggregator"` when the service forwards to
  upstreams (§1.2). A direct service has no upstream hop, so it publishes no
  attested sessions and its receipts carry no `upstream.verified` event.
- `service_capabilities.supported_multipart_constraint_endpoints` lists paths
  implementing the §5.3 multipart constraint carrier. Absent or empty means
  unsupported. Clients MUST obtain this capability over the attested,
  SPKI-pinned channel and accept the workload's code provenance; it is not
  independently committed by the hardware quote. A service MUST advertise a
  path only when it implements all carrier validation and enforcement rules.

### 4.2 Evidence

`tee_type` selects the evidence format: `tdx` means Intel TDX quote
verification, `sev_snp` means AMD SEV-SNP report verification, and any other
value requires a published verifier extension. The `evidence` object is
interpreted under the verifier policy. Under the dstack `tdx` policy, for
example, `evidence` carries the `quote`, its `quote_report_data`, the boot
`event_log` (a JSON-encoded RTMR event array), the booted `app_compose`,
the `vm_config`, KMS `key_custody`, and a `downstream_tls_binding` naming
which keyset TLS entry clients of this deployment pin — letting a verifier
replay the log to the quote's RTMR3 and match `sha256(app_compose)` to the
measured `compose-hash`.

When the keyset contains domain-scoped TLS entries, the client requests
the report through a hostname the keyset lists, so the SPKI it pins is the
one for the hostname it actually uses.

## 5. Inference Endpoints

ACI v1 covers inference endpoints, including OpenAI-compatible completions,
audio transcriptions, and formats such as Anthropic messages.
Requests and responses follow the underlying API unless a separately
specified transport extension says otherwise. ACI adds headers and artifacts.

| Endpoint | Status |
| --- | --- |
| `POST /v1/chat/completions` | REQUIRED |
| `POST /v1/completions` | OPTIONAL |
| `POST /v1/embeddings` | OPTIONAL (non-streaming only) |
| `POST /v1/images/generations` | OPTIONAL (non-streaming only; inline base64 output; request up to `n` images, 1..10, default 1; providers may return fewer; image charges use the returned count) |
| `POST /v1/audio/transcriptions` | OPTIONAL (`multipart/form-data`, non-streaming, `response_format` of `json` or `verbose_json`; constraints via §5.3) |
| Other prompt endpoints (e.g. OpenAI-format `/v1/responses`, Anthropic-format `/v1/messages`) | OPTIONAL (`/v1/responses` may be served through Chat Completions conversion or native passthrough) |
| `GET /v1/models` | OpenAI-compatible. ACI adds no required fields |

Trust metadata is service-level and lives in the attestation report. Clients
MUST NOT infer trust from `/v1/models` entries.

### 5.1 Request headers

| Header | When | Meaning |
| --- | --- | --- |
| `Authorization: Bearer <key>` | inherited | Service authentication. Also binds the receipt to this credential (§7.6). |
| `X-ACI-Provider: <JSON>` | multipart endpoints listed in `service_capabilities.supported_multipart_constraint_endpoints` | Serving constraints with the JSON `provider` schema (§5.3). |

### 5.2 Response headers

| Header | When | Meaning |
| --- | --- | --- |
| `X-ACI-Version: aci/1` | every response | Protocol version, including error responses. |
| `X-ACI-Keyset-Digest` | every response | The serving `workload_keyset_digest`. |
| `X-Receipt-Id` | inference responses and refusal errors (§7.5) | Lookup id for the signed receipt. |

An aggregator that commits a streaming response before selecting an
upstream (a keep-alive heartbeat ahead of the upstream's first byte)
cannot name a receipt in the header: such a response omits
`X-Receipt-Id`, and the receipt — issued once the stream finalizes — is
retrievable by the response's `id` (§7.2). A stream whose forward fails
after that early commit issues no receipt. Requests carrying §5.3
constraints are never committed early, so constrained clients always
see the header.

Headers are unauthenticated hints. Only the attested keyset and the signed
receipt bind anything. The header and receipt for one exchange carry the
digest established for that exchange. A changed digest gates new sends until
the client re-verifies according to §3.1; responses already in flight
continue unaffected.

### 5.3 Serving constraints (aggregators)

The JSON body of any prompt endpoint MAY carry a `provider` field with ACI
constraints:

```json
"provider": { "aci_verified": true, "aci_session_ids": ["<64-hex>", "..."] }
```

- `aci_verified: true` requires serving through a verified attested
  session (§8) — per-request `required` (§7.5). It only tightens: absent
  or `false` leaves the deployment's own setting (§1.2). Failure is the
  ordinary refusal (`upstream_verification_failed`). A direct service
  (§4.1) satisfies the constraint by construction — there is no second
  hop to attest.
- `aci_session_ids` requires serving through one of the listed sessions
  and implies `aci_verified`; combining it with `aci_verified: false` is
  invalid. The list MUST be a non-empty array of 64-hex session ids the
  client verified (§9.2). The service checks membership against the
  route's current sessions (including after a re-verification replaces
  them), nothing else. When none can serve, it refuses with
  `session_not_accepted` (§10) before forwarding, recorded like any other
  refusal (§7.5). A direct service has no sessions, so it refuses any
  list.
- The aggregator consumes these fields and MUST NOT forward them — they
  name its own sessions. Removing them appears as the §7.4 rewrite.
  Unknown `aci_`-prefixed fields are rejected (`invalid_request_error`),
  never ignored. The rest of `provider` is outside this specification.
- A transport extension defines how its protected request carries serving
  constraints and which restored bytes the `request.received` hash commits to.

For `multipart/form-data`, the carrier is a single HTTP header
`X-ACI-Provider: <JSON>`. Its value MUST be a JSON object using exactly the
same schema and semantics as the JSON `provider` field above, including
non-empty session lists, boolean `aci_verified`, and rejection of unknown
`aci_` members. The v1 multipart path is `POST /v1/audio/transcriptions`.
The field value MUST contain only US-ASCII bytes for HTTP
field interoperability (RFC 9110 §5.5); use JSON `\uXXXX` escapes, including
surrogate pairs where needed, for non-ASCII characters in strings.
The service MUST reject duplicate carrier headers, malformed JSON or a
non-object value, non-ASCII bytes, values larger than 8192 bytes, and any
multipart form part named `provider`, with HTTP 400 (`invalid_request_error`).
It MUST also reject the carrier on JSON endpoints or any endpoint not
supporting it.
The carrier is consumed in its entirety and MUST NOT be forwarded upstream.
Deployment-level verified-serving requirements apply independently.

Receipts commit to the raw multipart **body bytes**, not the contents of
`X-ACI-Provider`. Clients MUST intersect caller session pins with their local
accepted set, refuse disjoint sets, retain the effective verified-serving
requirement and session pins, and check the receipt's cited session against
them (§9.3). Caller constraints remain effective when local verified serving
is disabled. A successful body-hash check alone does not prove that header
constraints were honored. Before uploading constrained multipart, clients
MUST refuse locally unless the service advertises support for that exact
path, obtained under the capability trust rules in §4.1. When constrained,
malformed or non-object JSON and other non-empty body media types that cannot
carry constraints MUST likewise be refused locally.

## 6. E2EE Transport Extensions

> **Warning:** E2EE v2 is a temporary compatibility extension and will be
> replaced by E2EE v3. The reference gateway supports v2 through at least
> February 10, 2027. V2 clients should plan to migrate once v3 is specified.

ACI binds E2EE public keys into the workload keyset and advertises extension
versions, but the core `aci/1` specification does not define an E2EE wire
protocol or require one for conformance. Each version is specified separately.

The currently implemented compatibility extension is
[E2EE v2](e2ee-v2.md). Its headers, algorithms, encrypted fields, replay
rules, errors, receipt integration, and migration policy are defined only in
the E2EE v2 document.

## 7. Inference Receipts

A receipt is a signed, per-request event log. It binds the request bytes the
workload received, the bytes it forwarded, the upstream verification
outcome, and the response bytes it returned — all hashed inside the TEE and
signed with an attested receipt key.

### 7.1 Lookup

```text
GET /v1/aci/receipts/{id}
```

`{id}` is the `X-Receipt-Id` header value (preferred), or the
OpenAI-compatible response `id` when the response body contains one.
`X-Receipt-Id` arrives with the response, so the client holds the id before
the receipt is queryable. A receipt is finalized when the response
completes: a streamed response has no in-flight receipt (its hashes cover
the whole stream). Receipts are retained for a bounded,
implementation-defined period. Clients SHOULD fetch receipts promptly. An
unknown or expired id returns `not_found`.

### 7.2 Document and signature

The endpoint serves the receipt as one JSON document (§7.3). The
`signature` field signs the JCS form of the document without its
`signature` field (Appendix A). The verifier resolves `key_id` in the
established keyset's `receipt_signing_keys`, and that entry decides the
algorithm (§9.3): under the `ed25519` baseline, a 64-byte RFC 8032
signature, hex-encoded.

Any JSON encoding of the same document verifies, and the whole receipt is
one self-contained file a client can archive and re-verify offline.

### 7.3 Receipt document

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
    { "type": "upstream.verified", "...": "see §7.5" },
    { "type": "response.returned", "body_hash": "sha256:<hex>" }
  ],
  "key_id": "<receipt-key-id>",
  "signature": "<hex, §7.2>"
}
```

Receipts do not embed fresh attestation. They bind back to an
established keyset through `workload_keyset_digest` and the signing key. `model`
is the model id the client asked for. A transport extension defines how the
service extracts it from a protected request. Events are flat objects — `type`
plus type-specific fields — and event order is the array order. The first event
MUST be `request.received`.

For an aggregator receipt that records forwarding, `served_at` is the time of
the forwarding decision for the attempt whose upstream response the receipt
records. The service MUST admit that attempt against its current verification
state before forwarding. Response completion and receipt publication may occur
later, including after the cited session expires. For a refusal without
forwarding, `served_at` is the time the refusal is recorded.

### 7.4 Event vocabulary

All hashes are computed inside the TEE over bytes the workload actually
observed. Client-supplied hash headers are advisory and MUST NOT influence
receipt hashes.

| Event | Required | Fields | Meaning |
| --- | --- | --- | --- |
| `request.received` | yes, first | `body_hash` | The request body the workload processed. Plaintext requests hash the wire body. A transport extension defines the restored request bytes hashed after its protection is removed. |
| `request.forwarded` | if forwarded | `body_hash` | The exact bytes used for inference after any service-side rewrite (for an aggregator, the bytes forwarded upstream). A rewrite is this hash differing from `request.received`. Absent when the prompt was not forwarded (a §7.5 refusal). |
| `upstream.verified` | aggregator | §7.5 | The upstream verification outcome for this request (§7.5). |
| `response.returned` | yes | `body_hash` | The exact response body bytes emitted on the wire — for a §7.5 refusal, the error body served in place of an inference response. For SSE, the raw in-order stream including framing (`data:` lines, delimiters, terminating sentinel). |

Services MAY add events with implementation-specific types (the reference
implementation records routing decisions, for example), but MUST NOT reuse
the required types. Verifiers ignore event types they don't recognize
unless local policy cares.

### 7.5 `upstream.verified`

An aggregator receipt MUST contain an `upstream.verified` event
(additional events for other verification attempts MAY appear). A direct
service (§4.1) has no upstream hop, so its receipts carry none. Its two
forms:

```json
{ "type": "upstream.verified", "result": "verified",
  "required": true, "model_id": "<upstream model served>",
  "session_id": "<64-hex>" }

{ "type": "upstream.verified", "result": "failed",
  "required": true, "model_id": "<upstream model requested>",
  "reason": "<failure reason>", "upstream_name": "<optional label>" }
```

- `required` says whether the effective policy demanded verification for
  this request: `true` when the serving endpoint is TEE-only (§1.2) or the
  request carried the §5.3 constraint, `false` when neither applies — the
  request was served best-effort and the recorded result is informational.
  If required verification fails before dispatch, the service refuses to
  forward with `upstream_verification_failed` (§1.2). When no pinned session
  can serve, it refuses with `session_not_accepted` (§5.3). Either refusal
  error carries `X-Receipt-Id` (§5.2), and its receipt omits
  `request.forwarded` because nothing was dispatched.
- A verified event carries `session_id`, the content address of the attested
  session (§8) admitted for the forwarding attempt recorded by this receipt.
  The session contains the establishing verification's evidence and claims.
  The service MUST NOT substitute a session established after that forwarding
  decision.
- A failed event carries `reason` instead of a `session_id`, because the
  response cannot be attributed to a verified session. This can record a
  refusal before dispatch, best-effort serving with `required: false`, or
  an unauthenticated response after an admitted request was dispatched.
  In the last case, `required` still records the effective policy and the
  receipt MUST include `request.forwarded`. The failed event MUST NOT imply
  that nothing was forwarded or that an unauthenticated response was verified.

A verified event is the attested aggregator's assertion that its verification
policy authorized the forwarding attempt and that it enforced the cited
session's channel binding. Deep audit (§9.2) independently checks the
establishing evidence. It does not independently prove a later revalidation or
reconstruct the verification round that authorized a particular request.

### 7.6 Access control

Receipts contain hashes and verification metadata, never plaintext bodies.
A receipt for an authenticated request is protected by the same API key:
present the key that made the request to fetch its receipt (services
SHOULD store only a digest of the key for the comparison). Without a
credential the service returns `unauthorized`. With the wrong one it
returns `not_found`, exactly as if the receipt did not exist. Receipts for
unauthenticated requests MAY be publicly retrievable.

## 8. Attested Sessions

An aggregator verifies an upstream's attestation before trusting its channel
(§1.2). An attested session is an immutable snapshot of the verification used
to establish that channel: the upstream, verified identity and channel
material, claims, evidence, and validity period. Receipts cite this snapshot
by id. Only a verified TEE upstream yields a session; serving through an
upstream with no TEE appears as unverified, with no session to cite (§7.5).

Sessions are per verified channel and validity period, rather than per
request. A router serving several models through the same verified channel may
share one session; the receipt records the model served.

A service MAY reuse an unexpired session when a successful verification
establishes equivalent channel material. Equivalence MUST preserve the
verified workload identity, channel bindings, security-relevant measurements
and attributes, and recorded claims. Differences confined to
verification-round freshness or evidence encoding do not by themselves require
a new session. A verifier whose projection does not establish this equivalence
MUST treat changed evidence as changed material.

Reuse MUST NOT modify the session document or extend its `expires_at`. Changed
material cannot be served under the old session. After expiry, a subsequent
admission requires a new session. A temporary verification failure does not
revoke an immutable session; successful recovery with equivalent material may
reuse it while unexpired.

Session validity, verifier-cache freshness, and evidence age are distinct.
`established_at` records session creation, not necessarily evidence
collection. Creating a session from a currently usable cached verification
does not make its evidence newly collected. Every forwarding decision remains
subject to the service's current verification and channel-enforcement rules.

A session is immutable and content-addressed:

```text
session_id = hex(sha256(JCS(document)))
```

The id is not inside the document. The signed receipt commits to
`session_id`, so recomputing the id from the fetched document proves the
record is exactly what the receipt cited. There is no session
signature.

**Retention.** A session MUST remain retrievable, unchanged, for as long
as the service still serves any receipt citing it (§7.1), and SHOULD
remain available longer for receipts clients have archived. `expires_at`
ends the validity period for new forwarding decisions, not the retention
obligation.

An admitted request MUST keep its cited session available while its response
or receipt is pending. Publishing the receipt MUST retain that session through
the receipt's retention deadline. Keeping a session available does not extend
its validity for new forwarding decisions.

### 8.1 Endpoints

```text
GET /v1/aci/sessions/{session_id}           one session, full evidence
GET /v1/aci/sessions?upstream_name=&model=  list current sessions
```

`{session_id}` is the bare 64-hex id — no prefix — exactly as receipts
cite it, so the value from a receipt pastes straight into the URL. Sessions carry only
verification material, no request or response content, and MAY be served
without authentication as transparency artifacts.

The list endpoint is a convenience: a client can inspect the verified
identity, channel binding, and claims for a model before sending any
data (§9.2). It returns `{ "api_version": "aci/1", "sessions": [ ... ] }`.
`?model=` selects the sessions of the upstreams the service currently
maps to that model. List entries are abbreviated: each carries its
`session_id`, keeps `evidence.digest`, and drops the bulky
`evidence.data`. An abbreviated entry does not hash to its id — fetch the
full record to verify (§9.2).

### 8.2 Session record

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
  "claims": { "...": "§8.3" },
  "evidence": { "digest": "sha256:<hex>", "data": "data:<content-type>;base64,<...>" }
}
```

- `endpoint` is the verified upstream origin, or `null` when the channel
  has no single origin (for example a per-instance E2EE binding).
- `identity` records the verified identity keys of the upstream (for
  example a response-signing address), when the verifier established
  one. Its fields are verifier-specific.
- `evidence.data` is a data URI preserving the evidence bytes retained for the
  establishing verification, including a bundle when several inputs are
  required. `evidence.digest` is the SHA-256 of those decoded bytes. A
  verifier MUST reject a record whose data does not hash to its digest. Reuse
  of a session does not replace this evidence with evidence from later
  verification rounds.

`channel_binding` states what the aggregator enforced when it connected to
the upstream. Defined shapes:

```json
{ "type": "tls_spki_sha256",        "origin": "<https-origin>", "spki_sha256": "<hex>" }
{ "type": "e2ee_public_key_sha256", "provider": "<label>", "key_id": "<optional>", "algorithm": "<algo>", "public_key_sha256": "<hex>" }
```

A session may represent a logical TLS channel with several accepted keys. Its
`channel_binding` array then records the complete verified set of alternatives
enforced for that channel; a connection uses one of those alternatives.
Changing the set changes the session material. Distinct independently
selectable upstream instances remain distinct sessions.

A published extension may define another binding type; see Appendix B.

### 8.3 Typed claims

Claims state what was proven about an upstream in a fixed vocabulary that
keeps hardware-proven facts distinct from provider assertions. Each claim
is:

```text
{ "status": "asserted" | "refuted" | "unknown",
  "source": "hardware_proven" | "verifier_derived" | "provider_asserted" | "operator_asserted",
  "reason": "<verifier-supplied explanation>" }
```

`source` and `reason` are present only when `status` is not `unknown`.
Missing evidence is `unknown`: not a pass, not a refutation.

| Claim | Meaning |
| --- | --- |
| `tee_attested` | The channel terminates in a genuine CPU TEE with the recorded identity bound to it. |
| `gpu_attested` | A confidential-computing GPU attestation was verified for this channel. This attests the GPU exists and is genuine. It does not by itself prove the GPU is bound to the serving CPU TEE. |
| `tcb_up_to_date` | Platform TCB freshness as reported by the quote collateral. A stale TCB is `refuted`. |
| `os_known_good` | The platform/OS image maps to known-good provenance. |
| `serving_software_known_good` | The serving software maps to reviewed source or signed build artifacts. |
| `model_weights_provenance` | The served weights match their claimed provenance. |

An `extra` map MAY carry additional provider-scope facts verbatim (raw
verifier output such as `tcb_status`, `gpu_arch`, measurement values).
These are inputs to the typed claims, not claims themselves. The key names
inside `extra` are a stable contract for a given verifier: consumers may
depend on them, and a verifier MUST NOT rename or repurpose a published
key.

How a verifier establishes that GPU evidence is fresh and belongs to this
channel is its own policy (§1.3).

Receipts do not embed claims. They cite the session that carries them
(§7.5). §9.2 defines the shallow and deep audits over it.

## 9. Verification Procedure

Establish the service's identity once per keyset (§9.1). Everything
that protects a prompt happens before it is sent. Behind an aggregator, that
includes verifying the sessions you would accept (§9.2) and pinning them
on the request (§5.3). Check each response against its receipt, offline
(§9.3). An integration SHOULD say which of these it does — a keyset it
did not establish itself is only as good as its source.

### 9.1 Verify the workload identity

Under one verifier policy, check at minimum:

1. **Hardware.** The TEE evidence verifies to the vendor root and binds
   `report_data` (32 bytes, zero-padded to 64, in the report-data slot;
   §3.2).
2. **Binding and freshness.** The verifier MUST supply a fresh nonce.
   Recompute the chain: the SHA-256 of the keyset's JCS form (Appendix A)
   equals `workload_keyset_digest`; build the §3.2 statement from that
   digest and the supplied nonce; the SHA-256 of the statement equals
   `report_data`. One recomputation establishes that the keyset is
   exactly what the quote bound and that the quote postdates your
   challenge. The `nonce:null` form proves binding, not freshness.
3. **Expiry.** `now < not_after` in the keyset.
4. **Provenance.** The source provenance connects the attested workload to
   public code or build artifacts acceptable to the policy, corroborated
   by measured evidence — like the §4.2 compose-hash path. A provenance
   claim no measurement backs MUST NOT satisfy this check (§4.1).
5. **Custody.** Private-key custody for the listed keys satisfies the
   policy (§3.3), and `subject`, when present, is acceptable to it.
6. **Channel.** The channel actually used is bound: the observed TLS SPKI
   is listed in `tls_public_keys` (for the hostname used, when entries are
   domain-scoped), or the E2EE key used is listed in `e2ee_public_keys`.

Missing evidence required by the policy is fail-closed. Before sending
anything sensitive:

- Read the code, or rely on a reviewer you trust. The §1 privacy and
  integrity claims are enforced by the measured code, and the provenance
  (check 4) names it.
- From now on, talk to the service only through the keyset: TLS pinned to a
  listed SPKI, or content encrypted to a listed E2EE key under a separately
  specified extension (§6), on every connection. Before a new send, re-verify
  through the same single-flight path when `not_after` has passed or the
  served `X-ACI-Keyset-Digest` changes (§5.2). A renewing service changes its
  digest well before expiry, so a client normally re-verifies during the
  renewal according to §3.1 rather than cutting an in-flight response. Also
  re-establish it when your policy deny-lists the workload (§3.4).
- Behind an aggregator, the session list (§8.1) shows which upstreams
  currently back a model. Verify the ones you would accept (§9.2) before
  you send, and pin them with `aci_session_ids` (§5.3).

### 9.2 Verify a session (aggregators)

This section applies behind an aggregator. A direct service (§4.1) has no
sessions. Behind an aggregator, your prompt does not stop: it crosses the
aggregator's channel to an upstream and runs on the upstream's workload.
For a TEE upstream, the aggregator checks both before forwarding (§1.2)
and records what it checked in a session. There are two moments to
verify a session. Before you send, the list gives you candidates (§8.1).
After a response, your receipt cites the one that served you (§9.3). The
checks are the same:

1. Fetch the full record (`/v1/aci/sessions/{session_id}`) and recompute
   the id: `session_id` equals `hex(sha256(JCS(document)))` (§8), and
   `api_version` is `aci/1` (Appendix B).
2. `evidence.data` decodes and hashes to `evidence.digest`.
3. Shallow audit: the channel bindings and typed claims meet your policy
   — for example, `tee_attested` is `asserted` with source
   `hardware_proven`.
4. Deep audit: re-verify the establishing evidence under your policy for that
   provider, including its support for the recorded identity, bindings and
   claims. Evaluate freshness using the evidence and the provider's verification
   rules; session creation time alone is not evidence of collection time. Later
   revalidation is the aggregator's assertion (§7.5). A policy requiring
   independently verifiable evidence from each authorizing round MUST reject a
   session snapshot as insufficient for that requirement.

Pin the sessions that pass (`aci_session_ids`, §5.3). The service then
refuses to serve you through anything else, and the receipt's cited id
lets you confirm it (§9.3).

### 9.3 Verify the response

Before relying on a response, check that the service committed to
exactly the bytes you sent and the bytes you received. A kept receipt is
also your record of the exchange (§11).

Given an established keyset, plus a response and its receipt:

1. **Signature.** `signature` verifies over the JCS form of the receipt
   document without its `signature` field, under the key that `key_id`
   names in the established keyset's `receipt_signing_keys`, with that
   entry's algorithm (§7.2).
2. **Document.** The document's `api_version` is `aci/1` (Appendix B) and
   its `workload_keyset_digest` equals a digest the client established in
   §9.1 in the admitted identity's renewal family (§3.1), with that keyset
   unexpired at `served_at`. Verify the signature under that keyset. Current
   wall-clock expiry does not invalidate this historical receipt check.
   A client MAY perform a fresh verification to resolve an unseen digest,
   but MUST reject the receipt if that exact digest remains unestablished
   (§3.1). The client MAY
   retain established members for historical audits or discard them, in
   which case it cannot accept receipts whose necessary evidence it lacks.
3. **Request.** `request.received.body_hash` matches the wire body for a
   plaintext request. For a protected request, follow the advertised transport
   extension's receipt-integration rules (§6, §7.4).
4. **Response.** `response.returned.body_hash` matches the response bytes the
   client received off the wire, including the in-order raw SSE framing for a
   stream. When a transport extension protects the response, the client also
   performs that extension's authentication checks (§6).

Behind an aggregator, additionally:

5. The `upstream.verified` event has `result: "verified"` and cites a
   `session_id`. A client that requires verified serving rejects
   `"failed"` and `required: false`.
6. The cited session verifies (§9.2), ideally one you verified before
   sending. The receipt's `served_at` MUST satisfy `established_at <=
   served_at < expires_at`. Response completion need not fall within that
   window.
   If you pinned sessions (§5.3), the cited id is in your list.
   `served_at` is self-asserted (§11), so this catches an honest service
   citing an expired session. Against a dishonest one, the fail-closed
   rule (§1.2) rests on the attested code.

To see service-side rewrites, compare `request.forwarded.body_hash` with
`request.received.body_hash`: differing hashes are the rewrite. Whether a
rewrite is acceptable is local policy.

## 10. Errors

Errors use the OpenAI-compatible shape:

```json
{ "error": { "message": "...", "type": "<type>", "code": null, "param": null } }
```

Malformed non-ACI request members use the OpenAI-inherited types unchanged
(for example, an invalid §5.3 constraint is a 400 `invalid_request_error`).
ACI defines these types, with the HTTP status a service SHOULD use:

| Type | Status | Meaning |
| --- | --- | --- |
| `not_found` | 404 | Unknown or expired receipt / session id, or a credential mismatch (§7.6). |
| `unauthorized` | 401 | The receipt is credential-bound and no credential was presented. |
| `upstream_verification_failed` | 503 | Upstream verification was required and did not produce an enforceable verified binding. The prompt was not forwarded. |
| `session_not_accepted` | 412 | The request pinned sessions (§5.3) and none of them could serve it. The prompt was not forwarded. |

A service MAY use a different status where an HTTP intermediary requires it
(for example 429 for rate limiting), but SHOULD preserve the `type` so
clients can branch on it. Unrecognized types are treated as opaque, and
clients act on the status.

## 11. Security Considerations

Limits that remain after every §9 check passes:

- Every guarantee is enforced by the measured code itself. Verification
  identifies that code (§9.1(4)) but cannot vouch for it.
- No non-repudiation: `served_at` is self-asserted, and nothing orders or
  timestamps receipts. Durable proof needs an external transparency log.
- `gpu_attested` proves a genuine CC GPU, not its binding to the serving
  CPU TEE (§8.3).
- The service sees client IPs, credentials, and timing. An OHTTP relay
  (RFC 9458) in front hides the asker.

## 12. References

Normative for the wire formats in this document:

- RFC 9110 — HTTP semantics and field values.
- RFC 8032 — Ed25519 signatures.
- RFC 4648 — base64 encoding.
- RFC 8785 — JSON Canonicalization Scheme (JCS): the form the keyset,
  receipts, and sessions are hashed and signed in (Appendix A).
- Intel TDX and AMD SEV-SNP attestation documentation.

Referenced for architecture and composition:

- RFC 9334 — Remote ATtestation procedureS (RATS) architecture; RFC 9711 —
  Entity Attestation Token (EAT); draft-ietf-rats-ar4si — attestation
  results vocabulary.
- RFC 9458 — Oblivious HTTP, the composable metadata-privacy layer.
- RFC 9943 / RFC 9942 — SCITT architecture and COSE Receipts, the
  anticipated transparency-log binding.
- IETF SEAT working group — attested TLS, the anticipated stronger
  transport binding.
- NVIDIA attestation suite (NRAS, nvtrust) for GPU evidence.
- Sigstore, reproducible builds, and OpenSSF Model Signing as evidence
  formats for source and model provenance claims.
- dstack — KMS key custody and application identity model used by the
  reference implementation.
- [ACI Test Vectors](test-vectors.md) — byte-exact vectors for every
  digest and signature construction.
- [ACI and Related Work](related-work.md) — positioning against other
  confidential-inference systems.

## Appendix A. Artifact Conventions

Two rules cover every hash and signature in ACI:

1. **ACI's own JSON documents — the keyset, receipts, and sessions — are
   canonicalized, then verified.** Hashes and signatures are over the JCS
   form (RFC 8785) of the parsed document, so the served encoding is
   free: a service may pretty-print, and a verifier canonicalizes
   whatever it parsed, never checking how the server encoded it. Foreign
   bytes (HTTP bodies, `evidence.data`) are hashed exactly as observed.
2. **A verifier builds the attestation statement itself** (§3.2). The
   statement is a fixed template. Separately specified extensions define any
   additional verifier-constructed payloads (§6).

Under ACI's constraints (ASCII field names, integer numbers), JCS is just
compact JSON with lexicographically sorted field names.

Encodings:

| Value | Encoding |
| --- | --- |
| Digest fields (`workload_keyset_digest`, `body_hash`, `evidence.digest`) | `sha256:<lowercase-hex>` over the named bytes |
| Ids computed as hashes (`session_id`) | bare lowercase hex (how the id is computed is defined in §8) |
| `report_data` and fields ending in `_sha256` | bare lowercase hex |
| Fields ending in `_b64` | standard base64 of the exact underlying bytes (RFC 4648 §4, with padding) |
| Public keys and signatures | lowercase hex, no `0x` prefix |

Conventions:

- Domain separation: the attestation statement embeds the
  `aci.report_data.v1` purpose. Receipt signing needs no purpose string because
  receipt keys sign nothing else (§3.1 role separation). Extension documents
  define their own domain-separation values (§6).
- Some artifacts restate a field that is derivable elsewhere, so they are
  self-describing: the report carries the keyset digest beside the
  keyset, and a receipt names the digest that resolves its signing key. A
  verifier recomputes every restated field, and a mismatch is a
  failure. Artifacts never name a signature algorithm: the `key_id`
  resolves a keyset entry, and that attested entry decides it (§7.2).
- [Test vectors](test-vectors.md) pin every construction byte-for-byte.
- Extension points and every enumerated identifier live in Appendix B.

## Appendix B. Protocol Constants and Extension Points

Every identifier this version defines, in one place. A new value in any of
these sets requires a published extension document.

| Set | Values | Unknown value handling |
| --- | --- | --- |
| API version | `aci/1` (`api_version` fields, `X-ACI-Version` header) | Reject artifacts with other versions |
| Purpose / context strings | `aci.report_data.v1` | — (fixed statement tag) |
| Signature algorithms | `ed25519` baseline. Keysets may carry more (below) | Ignore a keyset entry whose `algo` is unknown. Reject an artifact signed with one |
| Receipt event types | `request.received`, `request.forwarded`, `response.returned`, `upstream.verified` | Ignore (§7.4) |
| Channel binding types | `tls_spki_sha256`, `e2ee_public_key_sha256` | Treat as not enforceable |
| Claim names | `tee_attested`, `gpu_attested`, `tcb_up_to_date`, `os_known_good`, `serving_software_known_good`, `model_weights_provenance` | Extra facts live in `claims.extra`. Unknown entries are informational |
| Claim statuses / sources | `asserted`, `refuted`, `unknown` / `hardware_proven`, `verifier_derived`, `provider_asserted`, `operator_asserted` | Treat the claim as `unknown` |
| TEE types | `tdx`, `sev_snp` | Requires a published verifier extension (§4.2) |
| Error types | §10 table | Treat as opaque. Act on HTTP status |
| Serving modes | `direct`, `aggregator` (`service_capabilities.serving`, §4.1) | Treat an unknown value as `aggregator` |
| Headers | §5.1, §5.2 tables | Ignore unrecognized `X-ACI-*` headers |
| Serving constraints | `provider.aci_verified`, `provider.aci_session_ids` (§5.3) | Reject unknown `aci_`-prefixed fields (`invalid_request_error`) |

Extension points:

- **Receipts** — services MAY add event types (§7.4). Verifiers ignore
  types they don't recognize unless local policy cares. The signature
  covers the whole document, so unknown events don't break verification.
- **Session records** — the `claims.extra` map (§8.3).
- **Channel bindings** — a published extension defines each further binding
  type (§8.2), such as [`proxy_image_sha256`](proxy-image-binding.md) for a
  provider proxy co-deployed in the aggregator's attested workload.
- **Non-ACI surfaces** — implementations MAY keep pre-ACI compatibility
  endpoints, headers, and report fields. These MUST NOT alter ACI
  artifacts: report, receipt, and session content, digests, and
  signatures are the same with or without them, and legacy report
  bindings use their own quotes rather than repurposing the §3.2
  statement. New clients and verifiers MUST use the `/v1/aci/*` endpoints
  and ignore compatibility fields.
- **Reports** — `attestation.evidence` is policy-defined, and consumers
  MUST ignore unrecognized `service_capabilities` fields.
- **The keyset** shape is fixed. New key roles need a new protocol version.
  Every verifier implements the `ed25519` baseline for receipt signatures.
  E2EE extensions define the algorithms they recognize in
  `e2ee_public_keys`. The attested entry picks the algorithm. A verifier skips
  entries it cannot implement (§3.1) and rejects an unknown algorithm instead
  of guessing. No negotiation, no downgrade.
