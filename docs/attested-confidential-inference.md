# Verification and security model

This page is for developers deciding whether an ACI deployment protects their
inference data. It explains the privacy claim, the evidence behind it, the
checks a client must perform, and the limits that remain.

The [ACI specification](../spec/aci.md) is normative. The
[quickstart](quickstart.md) is the runnable walkthrough.

## The privacy claim

For an accepted ACI request, remote plaintext is limited to the workloads that
must process it:

1. the attested gateway workload; and
2. the accepted provider workloads on the selected route, including a
   confidential router and model runner when they are separate.

The client verifies the gateway and its channel before sending the request.
The measured gateway code verifies the selected provider and enforces the
provider's attested channel binding before forwarding. A failed required check
stops the request before that hop receives the prompt.

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
| Optional external control plane | No prompt or response body | Bearer-token hash, model, routing options, pricing, usage, and status metadata |
| Cloud host and workload operator | Not through the accepted TEE memory boundary | Network timing, addresses, sizes, and operational metadata |

The gateway forwards the caller's routing object to the control plane as
metadata. Do not put prompts or secrets there. See the exact
[control-plane contract](control-plane-contract.md).

Provider workloads can have their own internal routing, telemetry, or storage
boundaries. Accept only the claims that the provider verifier actually proves.
The [provider verification index](providers/README.md) records those differences.

## The shortest verified path

Install the CLI as shown in the [quickstart](quickstart.md), then establish the
gateway identity:

```bash
aci verify https://tee.redpill.ai
```

The command obtains a fresh nonce-bound report and prints each pass, failure,
or skipped policy check. It exits successfully only when its implemented checks
produce a verified verdict.

To send one chat request and verify its response receipt and cited session:

```bash
export ACI_API_KEY=<your-api-key>
aci send https://tee.redpill.ai --prompt "What are you running on?"
```

Use `aci curl` when you need arbitrary curl behavior and a verified,
SPKI-pinned channel. It verifies before sending, but it does not audit the
response receipt. See the [CLI reference](../src/bin/aci/README.md) for the
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
not protect the request if TLS terminates somewhere else. The Node and Bun
runtime clients and the Rust CLI pin the observed certificate SPKI to the
attested keyset.

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
the cached result and triggers one fresh verification before the candidate
fails. The complete state machine is in
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
aci sessions https://tee.redpill.ai \
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

The current `aci` CLI verifies the DCAP quote, nonce/keyset binding, expiry,
RTMR3 compose measurement, and observed TLS SPKI. It reports private-key custody
as a skipped check and does not reconstruct the complete dstack boot chain.
`--require-production-os` appraises an RTMR3-bound OS image hash against a
reviewed allowlist; it is not a substitute for independently reconstructing
MRTD and RTMR0-2.

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
