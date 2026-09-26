# Verification and security model

This page is for developers deciding whether an ACI deployment protects their
inference data. It explains the privacy claim, the evidence behind it, the
checks a client must perform, and the limits that remain.

The [ACI specification](../spec/aci.md) is normative. The
[quickstart](quickstart.md) is the runnable walkthrough.

## The privacy claim

For an accepted ACI request, remote plaintext is limited to the workloads that
must process it:

1. the attested gateway workload, including its client-facing TLS terminator;
   and
2. the accepted provider workloads on the selected route, including a
   confidential router and model runner when they are separate.

The client verifies the gateway and its channel before sending the request.
The measured gateway code then verifies the selected provider and enforces the
provider's attested channel binding before forwarding. A failed required check
stops the request before that hop receives the prompt.

Two parts of this claim come from reviewing the measured release, not from a
check on the report:

- **Which code runs.** The report proves which compose booted. Whether that
  compose and the code it names are acceptable is your decision. You can pin
  reviewed compose hashes, rely on the operator to review its releases, or
  record the hash and review the release later.
- **Where private keys live.** The gateway derives its receipt and E2EE keys
  from dstack KMS inside the TEE, and `pap` can check the receipt key's KMS
  chain. The TLS private key has no such chain. The gateway serves plain HTTP
  behind a TLS terminator, and that key stays inside the TEE only if the
  reviewed compose runs the terminator and keeps its key there.

Under the TEE threat model, the gateway operator, model operator, and cloud host
cannot inspect protected workload memory. The local application still sees the
prompt and response. The accepted remote workloads also see plaintext because
they must process it.

This is not a promise from an API header. It is a policy decision based on
hardware evidence, measured software, attested keys, and enforced channels that
the relying party checks independently.

> [!IMPORTANT]
> Provider verification is a request constraint, not a global gateway mode.
> Set `provider.aci_verified: true`, pass a non-empty
> `provider.aci_session_ids` list, or use a TEE-only middleware hostname when a
> request must fail closed.

## Who receives what

| Component | Inference content | Other information |
| --- | --- | --- |
| Local client or agent | Plaintext prompt and response | API key and all local context |
| Attested gateway workload | Plaintext after TLS or E2EE termination | Requested model, credential, routing constraints, and provider response |
| Accepted provider workload or route | Plaintext needed for routing or inference | Gateway-side provider credential and request metadata |
| Optional external control plane | No prompt or response body | Bearer-token hash, model, routing options, request features including a prefix hash, usage, and status metadata |
| Cloud host and workload operator | Not through the accepted TEE memory boundary | Network timing, addresses, sizes, and operational metadata |

### What leaves the request path

The gateway does not log or report prompts, completions, or upstream error
messages. Of an upstream response body, only its `usage` object is reported.
This table lists everything that leaves the request path, so you can check it
against the code:

| Destination | Contents | Defined in |
| --- | --- | --- |
| Pre-request consult to the control plane | SHA-256 of the API key, the requested model, the caller's `provider` routing object, the TEE-only host flag, and, unless `middleware.send_request_features` is `false`, request features: a token estimate, input modalities, tool and response-format flags, reasoning intent, and `prefixHash` | `consult_pre` in `src/middleware/control.rs`, `src/middleware/request_features.rs` |
| Usage report to the control plane, one per attempt | Request ID, endpoint, status, timings, streaming flag, attempt index, selected route, requested model, the upstream's `usage` object, the consult's routing and billing fields echoed back unchanged, `errorSource`, a failure class in `errorMessage`, and `prefixHash` | `PostReport` in `src/middleware/types.rs` |
| Receipt | SHA-256 hashes of the request and response bodies, never the bodies | `src/aci/receipt.rs` |

Two properties make this checkable:

- `errorMessage` carries no message text. Its type is `ErrorClass`, an enum
  with no string-bearing variant, so the value is one token from a closed list,
  such as `upstream_timeout` or `stream_truncated`. An upstream error body is
  read only to choose a class (`classify_upstream` in
  `src/middleware/errors.rs`).
- `tests/middleware_completion.rs` plants a marker in upstream error text on the
  buffered, streaming, and in-band stream paths. It captures every log event at
  `TRACE` and fails if the marker appears in any log line or usage report.

The requested model name and the caller's `provider` object are forwarded as
the caller sent them, so do not put secrets there. The `usage` object is the
upstream's, forwarded as received.

The gateway has no access log and no per-request outcome line. It logs its own
errors, such as a failed control-plane call, an upstream timeout, or a response
that fails partway (`stream_abort`, the one line that carries a request ID).
Such a line can name the requested model or the request host. It never carries
request or response content.

`prefixHash` is derived from content: an HMAC-SHA256 of the conversation's
first 4 KiB, truncated to 32 hex characters, used for cache-affinity routing.
The gateway derives the HMAC key from dstack KMS, so the key never leaves the
TEE. The control plane can see when two requests share a prefix, but it cannot
test a guessed prefix against the hash. See the
[control-plane contract](control-plane-contract.md) for every field.

Provider workloads can have their own internal routing, telemetry, or storage
boundaries. Accept only the claims that the provider verifier actually proves.
The [provider verification index](providers/README.md) records those differences.

## The shortest verified path

Install the CLI as shown in the [quickstart](quickstart.md), then verify the
gateway:

```bash
pap verify https://tee.redpill.ai
```

The command obtains a fresh nonce-bound report and prints each pass, failure,
or skipped check. It exits successfully only on a `VERIFIED` verdict. Without
`--accept-compose`, a pass means genuine TEE hardware booted the compose whose
hash the transcript prints. It does not mean you approved that release. See
[Choose what you accept](quickstart.md#choose-what-you-accept).

To send one chat request and verify its response receipt and cited session:

```bash
export ACI_API_KEY=<your-api-key>
pap send https://tee.redpill.ai --prompt "What are you running on?"
```

Use `pap curl` when you need a one-request curl command and a verified,
SPKI-pinned channel. It verifies before sending, but it does not audit the
response receipt. See the [CLI reference](../apps/desktop/docs/cli.md) for the
differences among `verify`, `curl`, `send`, `sessions`, `audit`, and `serve`.

## How privacy is enforced

ACI protects the path in three stages.

### 1. Before the client sends data

The client fetches `GET /v1/aci/attestation` with a fresh random nonce and
checks the ACI §9.1 chain:

1. The hardware quote verifies to an accepted TEE vendor root.
2. The quote binds `report_data`, which binds the nonce and the digest of the
   served workload keyset.
3. The keyset has not expired.
4. Measured evidence supports source or release provenance accepted by policy.
5. Private-key custody satisfies the relying party's policy.
6. The channel used for inference terminates at a TLS or E2EE key in that
   attested keyset.

The last check matters. A valid quote beside an ordinary HTTPS connection does
not protect the request if TLS terminates outside the accepted workload. The
Rust CLI and the Node and Bun runtime clients pin the connection to the TLS key
the attested keyset lists for that host. The pin proves which key the
connection used. That the key's private half stays in the TEE follows from the
reviewed compose, as described in [The privacy claim](#the-privacy-claim).

The browser verifier can check the quote, binding chain, measurement, receipts,
and sessions. Browser APIs do not expose the peer certificate, so browser-only
code cannot enforce the TLS SPKI pin.

### 2. Before the gateway forwards data

For a request that requires verified serving, the attested gateway:

1. selects a provider candidate;
2. runs or reuses that provider's verifier under its configured policy;
3. requires a verified result;
4. applies any client-supplied session allowlist;
5. opens a connection that enforces the verified TLS SPKI or provider E2EE
   public key; and
6. forwards the prompt only after that binding succeeds.

Each candidate is checked independently. A verified event for one origin,
model scope, or channel never authorizes another. A binding mismatch invalidates
the cached result and triggers up to two fresh verifications before the
candidate fails. The complete state machine is in
[Upstream verification lifecycle](upstream-verification-lifecycle.md).

Without an ACI constraint, a provider verification failure may be recorded as
informational while the request continues. That mode is useful for mixed
confidential and ordinary routing, but it is not a fail-closed privacy claim.

### 3. After the response

The gateway signs a per-request receipt under a key from its attested keyset.
The receipt commits to:

- the request body the gateway processed;
- the body forwarded after any gateway rewrite;
- the upstream verification result and cited session;
- the exact response bytes returned, including raw SSE framing; and
- the gateway keyset digest current for the exchange.

The receipt stores hashes, not plaintext request or response bodies. For an
aggregator, its `upstream.verified` event cites a content-addressed attested
session. The session preserves the provider channel binding, typed claims, and
evidence used by the verifier.

A receipt proves what the attested gateway recorded. A deep session audit lets
the relying party reappraise the provider evidence instead of accepting the
gateway's `verified` label alone.

### Audit the receipt

Given an established workload keyset and the exact request and response bytes,
a relying party checks:

1. The receipt `signature` verifies over the JCS form of the document without
   its `signature` member, using the `receipt_signing_keys` entry named by
   `key_id`.
2. `api_version` is `aci/1`, and `workload_keyset_digest` matches the
   established gateway identity.
3. `request.received.body_hash` matches the plaintext request wire body. For
   E2EE v2, it instead matches the compact JSON body reconstructed after
   replacing encrypted fields with their decrypted values.
4. `response.returned.body_hash` matches the exact bytes received from the
   wire, including ordered SSE framing and any encrypted response fields.
5. A required aggregated request has an `upstream.verified` event with
   `required: true`, `result: "verified"`, and a `session_id`.
6. The full session hashes to that ID, its evidence hashes to
   `evidence.digest`, the receipt's `served_at` falls inside its validity
   window, and its claims satisfy local policy.

Compare `request.forwarded.body_hash` with `request.received.body_hash` to
detect a gateway rewrite. Whether that rewrite is acceptable remains local
policy. A missing or failed required check makes the response unacceptable.

Fail-closed verification and session-pin refusals also carry `X-Receipt-Id`.
Their receipts commit to the original request, the failed upstream event, and
the exact error body, so a client can verify that the attested gateway refused
before forwarding.

## Proof layers

| Layer | Artifact | What it proves | What it does not prove by itself |
| --- | --- | --- | --- |
| Gateway identity | Attestation report | Fresh TEE evidence binds the gateway keyset and measured workload | That the relying party approves the measured release |
| Client channel | Observed TLS certificate or E2EE key | Inference bytes terminate at a key in the attested keyset | Provider-side privacy after the gateway |
| Provider path | Attested session | The gateway verified and enforced a provider binding with recorded evidence | Claims the provider verifier left `unknown` |
| Exchange integrity | Signed receipt | Request, rewrite, response, and serving record are bound to the attested gateway | An external timestamp or non-repudiable public log |

Every layer has a different job. An `x-receipt-id` alone proves nothing until
the client fetches the receipt, verifies its signature and body hashes, and
checks the cited session under its own policy.

## Pin an upstream session

A client can inspect current sessions before sending sensitive data:

```bash
pap sessions https://tee.redpill.ai \
  --require-claim tee_attested=hardware_proven
```

The command fetches each full record, recomputes its content address, validates
the evidence digest and validity window, and applies the requested claims
policy. Pass the accepted IDs on the inference request:

```json
{
  "provider": {
    "aci_verified": true,
    "aci_session_ids": ["<accepted-session-id>"]
  }
}
```

The gateway consumes this object and does not forward it upstream. If no listed
session can serve, it returns `session_not_accepted` before forwarding. A
binding rotation creates a new session ID, so an old pin cannot silently accept
the new channel.

## Artifact endpoints

| Endpoint | Purpose |
| --- | --- |
| `GET /v1/aci/attestation?nonce=<64-hex>` | Fresh gateway attestation report and workload keyset |
| `GET /v1/aci/receipts/{id}` | Signed receipt, fetched with the request credential when authentication was used |
| `GET /v1/aci/sessions/{session_id}` | Full immutable upstream session and evidence |
| `GET /v1/aci/sessions?upstream_name=&model=` | Current abbreviated sessions for inspection before a request |

Receipts are retained for a bounded time and should be fetched promptly. The
reference implementation keeps them in memory for one hour and loses them on
restart. A session must remain available while a retained receipt cites it,
but the reference session store is not an externally witnessed transparency
log.

See the [HTTP API reference](api-reference.md) for authentication, response
shapes, and legacy aliases.

## E2EE v2

E2EE v2 is an optional compatibility extension that encrypts supported content
fields between the client and the attested gateway workload. The quote-bound
`e2ee_public_keys` entry is the key-provenance anchor. Field AAD binds the
ciphertext to its model, field path, nonce, timestamp, direction, and response
ID.

E2EE v2 protects content across infrastructure in front of the gateway TEE. It
does not hide content from accepted workloads on the inference path, and it
does not define encryption for every API field or endpoint. It covers Chat
Completions, Completions, and Embeddings as specified in the
[E2EE v2 protocol](../spec/e2ee-v2.md).

The extension is frozen and supported through at least February 10, 2027.
E2EE v3 is the planned replacement.

## Build a verifier policy

The same report is not automatically acceptable to every user. A production
policy should state at least:

| Policy input | Decision to make |
| --- | --- |
| Hardware roots and TCB states | Which TEE vendors, collateral sources, debug states, and TCB statuses are accepted? |
| Boot and OS measurements | Which dstack or other platform images are accepted, and how are their measurements reconstructed? |
| Workload release | Which RTMR3-bound compose hashes or equivalent measured releases were reviewed? |
| Source provenance | How does the measured artifact map to public source and build provenance? |
| Key custody | Which KMS roots and derivation chains establish custody for receipt, E2EE, and TLS keys? |
| Provider evidence | Which verifier versions, channels, claim sources, and model paths are accepted? |
| Rotation and expiry | How are overlapping releases, keysets, and session changes handled? |

The `pap` CLI verifies the DCAP quote, the nonce and keyset binding, expiry,
the RTMR3 compose measurement, and the TLS key the report declares for the
host. Policy flags narrow what it accepts:

| Flag | Policy input it enforces |
| --- | --- |
| `--accept-compose <hash>` | Workload release |
| `--accept-subject app-id:0x<hex>` with `--accept-dstack-kms-root-public-key <key>` | Key custody for the receipt-signing key |
| `--require-production-os` | An RTMR3-bound OS image hash on a reviewed allowlist |

Without a custody policy, the CLI reports custody as skipped. It does not
reconstruct MRTD and RTMR0-2, so `--require-production-os` does not replace a
dstack verifier run over the same quote, event log, and VM configuration. The
TypeScript verifier does not check custody.

Do not turn an unproven field into a stronger claim. A reported repository,
commit, image, model ID, or TCB status remains a label until the applicable
measurement and policy corroborate it.

## Non-goals and remaining exposure

Even when every applicable check passes:

- The measured code is trusted to implement the privacy policy correctly.
  Attestation identifies code; it does not prove that the code is bug-free.
- Exact model-weight provenance remains unknown unless the provider verifier
  supplies and checks suitable evidence.
- The service sees network and account metadata. ACI does not hide client IP,
  timing, request size, model choice, or credential use. An OHTTP relay is a
  separate metadata-privacy layer.
- Receipts are self-timed records, not externally ordered or timestamped
  statements. Durable non-repudiation needs a transparency service.
- GPU attestation proves properties of a GPU only to the extent recorded by
  the provider verifier. It may not prove a hardware binding between that GPU
  and the serving CPU TEE.
- Local agents, tools, MCP servers, browser automation, shell commands, and
  telemetry can expose data outside the model HTTP path.
- Availability is not guaranteed. Failing closed can turn verifier, collateral,
  or channel failures into a service outage.

## Legacy compatibility

`GET /v1/attestation/report`, `GET /v1/signature/{id}`, and the
`X-Signing-Algo` E2EE mode remain for pre-ACI dstack-vllm-proxy clients. They
use separate report bindings and do not alter canonical ACI reports, receipts,
or sessions. New verifiers should use `/v1/aci/*`.

## Continue

- Run the [ACI quickstart](quickstart.md).
- Compare current [provider verification](providers/README.md).
- Inspect the [attested-session system](attested-session-system.md).
- Read the normative [ACI specification](../spec/aci.md).
- Track known [implementation gaps](reviews/aci-spec-conformance-gaps.md).
