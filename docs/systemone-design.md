# System One endpoint: design proposal

Status: proposed; not implemented.

Reviewed: 2026-10-02. Gateway revision: `ef26e16d222a238e68633dd3522fde9f62ddae4f`.

This maintainer record proposes `POST /v1/systemone` for native structured
decision models. It is not an API reference, an ACI protocol change, or a claim
that any model is available in a confidential deployment. The
[HTTP API reference](api-reference.md) remains the source for implemented routes.

## Existing work and compatibility target

At the reviewed revision, the router, endpoint enum, and middleware have no
System One implementation. A 2026-10-02 search of this repository's GitHub issues
and pull requests for `systemone`, `system one`, `jev`, `kev`, and `decision model`
found no matching feature work. Remote branch names and the active project
workspaces did not identify another implementation. These observations do not
rule out unpublished work.

The external protocol already exists:

- [TypeSafe's HTTP API](https://docs.typesafe.ai/api) accepts state and typed
  questions at `POST /v1/systemone`.
- [OpenRouter's System One API](https://openrouter.ai/docs/api/api-reference/systemone/submit-a-system-one-request)
  exposes the same success contract at `POST /api/v1/systemone`.
- [Kev](https://github.com/jaredpalmer/kev) provides an open-weight, Apache-2.0
  model family and a server that accepts TypeSafe SDK decision calls.

The proposed target is the native HTTP decision contract and the TypeSafe SDK's
decision calls. Full SDK compatibility, including model discovery and error
handling, needs separate verification; it must not be inferred from a matching
success body.

## Decision and scope

Implement an explicit System One endpoint, not a Chat Completions wrapper that
asks a language model to write probabilities. Start with Kev-4B as the candidate
for a self-hosted evaluation and use Jev as an optional external comparison.
Neither selection admits a provider or establishes a TEE deployment.

The first implementation should support:

- buffered requests containing any mixture of `choice`, `score`, and `noul`;
- direct-upstream and control-plane middleware modes;
- native request forwarding, bounded response validation, and existing receipts;
- existing ACI request constraints and TEE-only hostname enforcement;
- native usage accounting and credential-free HTTP contract tests.

Out of scope are streaming, an E2EE profile, Chat-to-System-One conversion,
model training, automatic decision thresholds, new receipt fields, production
deployment, and changing global defaults for existing inference endpoints.

## Proposed client contract

### Request

Use Bearer authentication and JSON as on the existing inference endpoints. The
native body has three required fields:

| Field | Proposed handling |
| --- | --- |
| `model` | Non-empty public model ID, resolved by the existing routing layer. |
| `state` | A string, JSON object, or array. Preserve structured content. |
| `questions` | A non-empty map of question IDs to typed question objects. Preserve IDs exactly. |

`provider` remains a gateway routing extension. Its ACI constraints follow the
[existing rules](api-reference.md#require-aci-verification) and must not reach a
native model server as inference content. Other provider-routing fields retain
their existing control-plane semantics.

Each question requires `type` and `instructions`. Instructions and rubric
descriptions can contain strings, objects, or arrays; do not flatten them into
chat messages. The primitive contracts are:

| Type | Criteria | Answer |
| --- | --- | --- |
| [`choice`](https://docs.typesafe.ai/primitives/choice) | Option-name map; descriptions may also be `null`. TypeSafe documents a maximum of 255 options. | Selected option, probabilities for all options, and confidence. |
| [`score`](https://docs.typesafe.ai/primitives/score) | Ordered array of 2–10 rubric descriptions. | Probability-weighted, zero-based score, legend, probabilities, and confidence. |
| [`noul`](https://docs.typesafe.ai/primitives/noul) | Optional descriptions under `true` and `false`. | Probability of yes, between 0 and 1. |

TypeSafe's limits are not evidence that every backend supports those limits.
Apply reviewed backend limits without silently truncating questions or options.
Confirm edge cases, including single-option Choice requests, against pinned SDK
and backend fixtures before declaring full compatibility.

Reject unknown primitive types and malformed fields before consulting or
forwarding. Define an explicit allowlist for endpoint-level fields; arbitrary
provider extensions must not become executable or routing parameters. The MVP
may accept `stream: false` as a compatibility no-op but rejects `stream: true`
and non-boolean values. Reject E2EE headers before attempting chat-field
decryption. Reuse the current 32 MiB request-body limit.

This is an illustrative future request, not a runnable example against today's
gateway. `kev-4b` would need an explicitly configured catalog entry:

```json
{
  "model": "kev-4b",
  "state": {"message": "The account export fails for every user."},
  "questions": {
    "team": {
      "type": "choice",
      "instructions": "Which team should investigate?",
      "criteria": {
        "platform": "Service outages and failed account operations",
        "billing": "Charges, invoices, and refunds"
      }
    },
    "urgent": {
      "type": "noul",
      "instructions": "Is a core account operation unavailable?"
    },
    "impact": {
      "type": "score",
      "instructions": "Rate the operational impact.",
      "criteria": ["Cosmetic issue", "Degraded operation", "Blocked operation"]
    }
  },
  "provider": {"aci_verified": true}
}
```

### Response

Return the native `model`, `answers`, and `usage` shape. Do not introduce
`choices`, a chat completion ID, or a generated explanation. Validate a buffered
upstream success before returning it:

- `answers` has exactly the requested IDs and matching primitive types;
- every probability and confidence is finite and lies in `[0, 1]`;
- Choice distributions contain exactly the supplied options, sum to one within
  a documented floating-point tolerance, and select a maximum-probability option;
- Score distributions and legends cover the supplied zero-based levels; the
  score agrees with the weighted mean within a documented tolerance;
- `usage.input_tokens` and `usage.output_tokens` are non-negative integers.

Retain structured legend descriptions rather than assuming every description
is a string. Bound the buffered upstream response as well as the request; an
oversized or invalid response is an upstream failure, not a partial success.
Do not silently renormalize probabilities or fabricate missing answers.

Use a native allowlist to remove unrelated upstream diagnostics. Preserve the
upstream's `model`, including a resolved version when one is reported. If a
backend reports only an alias, do not invent an immutable model identity.
[TypeSafe's model documentation](https://docs.typesafe.ai/models) explains why
alias resolution matters for reproducibility and threshold changes.

Pass confidence through unchanged. It is a
[distribution-derived statistic](https://docs.typesafe.ai/confidence), not a
guarantee of correctness or an interchangeable maximum probability.

Keep actual token counters even when the configured output price is zero. Reuse
pricing and the gateway-owned `usage.cost` extension; do not assume that no
autoregressive decoding means `output_tokens` is always zero. Post-consult
receives validated provider usage before client cost injection.

### Errors and retries

Keep gateway errors sanitized and distinguish a malformed client request from
an invalid upstream success. Use `400` for malformed JSON and unsupported
streaming or E2EE, `422` for invalid question structure, and `413` for the body
limit. Unknown models and missing native routes use the existing routing errors.

An invalid upstream success produces a bounded, sanitized upstream failure
(`502` if no eligible candidate succeeds). Preserve the existing ACI refusal
statuses, receipt ownership, and channel-binding failure behavior. Reuse the
gateway's upstream-status mapping rather than relaying provider error bodies;
in particular, explicitly test TypeSafe's `529` overload case and its safe
gateway mapping with the SDK retry behavior.

Any candidate failover must remain bounded, follow the existing ordered policy,
and retain the caller's ACI constraints. A retry can consume provider quota;
the new endpoint does not promise exactly-once inference or a new idempotency
mechanism.

## Integration with this gateway

### HTTP and middleware

Add a path constant, handler, and `Endpoint::SystemOne`. Reuse the common body
limit, caller identity, ACI constraint extraction, middleware authorization,
forwarding, metering, and receipt finalization. Avoid a second HTTP proxy stack.

The endpoint needs its own shaping and response branch. The current default in
[`handlers.rs`](../src/http/app/handlers.rs) selects Chat Completions for an
unrecognized middleware endpoint. The chat allowlists in
[`response_transform.rs`](../src/middleware/response_transform.rs) would remove
`answers` and native usage counters; its identity rewrite would also replace
the upstream model with the requested alias. None of those chat operations
should run for System One. Direct mode must perform the same boundary checks
as middleware mode.

### Control-plane contract

The proposed additive changes to
[`/consult/pre`](control-plane-contract.md#post-consultpre) are:

- send `endpoint: "/v1/systemone"` for the new endpoint; omit the new field for
  existing endpoints to preserve their current request shape;
- add `format: "systemone"` to the candidate format vocabulary;
- require a native candidate to list `/v1/systemone` in `supportedEndpoints`;
- omit chat request features and `prefixHash` initially rather than reporting
  fabricated token estimates or hashing raw state into the control plane.

The gateway rejects or skips candidates that lack the native format and path.
It must never fall back to Chat Completions, apply chat reasoning policies, or
inject chat engine parameters. Conversely, System One candidates must not serve
chat requests. A custom runtime can internally use SGLang or another engine
without receiving the gateway's chat-specific engine transformations.

`format` identifies a wire contract, not an attestation provider. Do not add a
new `UpstreamProvider` variant just for Jev or Kev: reuse an appropriate existing
transport and verification adapter only when its actual contract is satisfied.

Update the reference control plane's typed configuration, candidate validation,
and model catalog before enabling middleware traffic. The current post-consult
report already contains `endpoint`; preserve its failure classification and
send no raw state, questions, answers, or diagnostic error text.

### Catalog and SDK boundary

Advertise only configured models with a usable native route, and keep TEE-only
catalog filtering. A generic decision-model label is not enough to prove
endpoint compatibility.

Preserve the existing OpenAI `GET /v1/models` contract. TypeSafe model discovery
expects a `models` array with a different entry shape. Decide and test a
non-breaking discovery adapter separately; the first endpoint implementation
must not claim that every SDK method works after changing only the base URL.

### ACI and encryption

API compatibility, open weights, and a model name do not prove confidential
serving. A route requires an accepted verifier result and an enforced binding
when `provider.aci_verified`, a session allowlist, or a TEE-only hostname demands
it. A missing verifier or an unenforceable binding must fail before forwarding.
No plain TypeSafe or OpenRouter route should be marked verified by assumption.

Keep request observations before stripping gateway fields, record the actual
provider-facing request, and bind the exact returned bytes to the existing
receipt. Preserve the upstream session and signed refusal path. Receipt lookup
uses `X-Receipt-Id`; a native response need not have a chat ID. No change to
the [ACI receipt schema](../spec/aci.md#7-inference-receipts) is proposed.

E2EE v2 has no reviewed System One field profile. Reject its headers with
`e2ee_unsupported_endpoint`, as on unsupported existing surfaces. A future
profile would require coordinated specification, implementation, vectors, and
client changes under [Contributing](../CONTRIBUTING.md#aci-wire-or-cryptography).

## Implementation slices and acceptance

| Slice | Deliverable and evidence |
| --- | --- |
| Native endpoint | Typed boundary validation, bounded buffering, native success shape, and credential-free mock-upstream tests in direct mode. |
| Middleware | Native format and endpoint capability checks, reference control-plane updates, ordered failure handling, pricing, and content-free reports. |
| ACI regression coverage | Verified success, missing verifier, binding mismatch, rejected session ID, TEE-only downgrade attempt, receipt hashes, and ownership checks. |
| SDK compatibility | Mixed-primitive decision calls using pinned official Python and JavaScript SDK versions against a local fixture, including structured criteria, aliases, errors, and retries. Record the tested versions; settle discovery separately. |
| Model evaluation | Separately authorized, pinned Kev serving evaluation and optional billed Jev comparison. No production change is implied by the endpoint. |

Contract tests should also cover option order, absent or extra answer IDs,
unknown selected options, incorrect probability sums, invalid scores, negative
token counters, oversized upstream bodies, unsupported streaming, and E2EE
rejection. Existing chat, Messages, Responses, embeddings, catalogs, and receipt
tests must remain unchanged in behavior.

A model evaluation should report accuracy, probability calibration, selective
automation at a chosen error budget, Chinese-language behavior, context-length
sensitivity, P50/P95 latency, and cost on the same inputs. Keep those dated
results separate from this endpoint design and from provider admission evidence.

When implementation lands, update the living
[HTTP API reference](api-reference.md),
[control-plane contract](control-plane-contract.md), and any changed
[configuration fields](configuration-reference.md) in the same change. Move the
feature out of the [roadmap](roadmap.md) only when code, tests, and those
references agree. A confidential deployment additionally needs the applicable
[provider review](providers/audit-criteria.md) and verification notes.
