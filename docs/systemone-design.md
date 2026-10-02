# System One endpoint: design proposal

Status: proposed; not implemented.

Reviewed: 2026-10-02. Gateway revision: `ef26e16d222a238e68633dd3522fde9f62ddae4f`.

This maintainer record proposes `POST /v1/systemone` for native structured
decision models. It is not an API reference, an ACI protocol change, or a claim
that any model is available in a confidential deployment. The
[HTTP API reference](api-reference.md) remains the source for implemented routes.

Design constraint: reuse the existing inference pipeline. Add the route, native
field tables, and a System One support check in the existing candidate loop.
Do not add a separate decision engine, business validator, capability framework,
middleware format, or control-plane API.

## Existing work and compatibility target

At the reviewed revision, the router, endpoint enum, and middleware have no
System One implementation. A 2026-10-02 search of this repository's GitHub issues
and pull requests for `systemone`, `system one`, `jev`, `kev`, and `decision model`,
before this proposal was published, found no matching feature work. Remote
branch names and the active project workspaces did not identify another
implementation. These observations do not
rule out unpublished work.

The external protocol already exists:

- [TypeSafe's HTTP API](https://docs.typesafe.ai/api) accepts state and typed
  questions at `POST /v1/systemone`.
- [OpenRouter's System One API](https://openrouter.ai/docs/api/api-reference/systemone/submit-a-system-one-request)
  exposes the core decision contract at `POST /api/v1/systemone`, with extensions
  such as request `session_id`, `user`, `provider`, and `trace`, and response
  `id`, `provider`, and `usage.cost`.
- [Kev](https://github.com/jaredpalmer/kev) provides an open-weight, Apache-2.0
  model family and a server that accepts official TypeSafe Python SDK decision
  calls.

The proposed target is the core decision request and answer shapes through
the gateway's existing policies, not every provider extension. Full SDK
compatibility, including model identity, discovery, and error handling, needs
verification; it must not be inferred from a matching success body.

## Decision and scope

Implement System One as another endpoint of the existing gateway, like
Embeddings and Responses. Preserve its native fields instead of converting
questions to chat prompts. Model evaluation and deployment are separate work;
this proposal does not select or admit a confidential provider.

The first implementation should support:

- buffered requests containing any mixture of `choice`, `score`, and `noul`;
- direct-upstream and control-plane middleware modes;
- native fields through the shared transforms, accounting, and receipts;
- existing ACI request constraints and TEE-only hostname enforcement;
- native usage accounting and credential-free HTTP contract tests.

Out of scope are streaming, an E2EE profile, Chat-to-System-One conversion,
model training or evaluation, automatic decision thresholds, new receipt fields,
new provider formats, control-plane wire changes, production deployment, and
changing global defaults for existing inference endpoints.

## Proposed client contract

### Request

Use Bearer authentication and JSON as on the existing inference endpoints. The
native body has three required fields:

| Field | Proposed handling |
| --- | --- |
| `model` | Non-empty public model ID, resolved by the existing routing layer. |
| `state` | A string, JSON object, or array. Preserve structured content. |
| `questions` | A non-empty map of question IDs to typed question objects. Preserve IDs exactly. |

`provider` remains the existing gateway routing extension. Its ACI constraints
follow the [existing rules](api-reference.md#require-aci-verification), including
the current extraction and stripping behavior in each topology. Do not add a
System One routing block or change how other provider fields are handled.

Each question identifies its primitive with `type`. Instructions and rubric
descriptions can contain strings, objects, or arrays; do not flatten them into
chat messages. TypeSafe requires `instructions`; Kev makes it optional. The
published primitive contracts are:

| Type | Criteria | Answer |
| --- | --- | --- |
| [`choice`](https://docs.typesafe.ai/primitives/choice) | Option-name map; descriptions may also be `null`. TypeSafe documents a maximum of 255 options. | Selected option, probabilities for all options, and confidence. |
| [`score`](https://docs.typesafe.ai/primitives/score) | Ordered rubric descriptions; TypeSafe documents 2–10 levels, Kev supports 1–255. | Probability-weighted, zero-based score, legend, probabilities, and confidence. |
| [`noul`](https://docs.typesafe.ai/primitives/noul) | Optional descriptions under `true` and `false`. | Probability of yes, between 0 and 1. |

These are upstream protocol descriptions, not new gateway-wide limits. Shape
`model`, `state`, and `questions` through the existing parameter-config mechanism,
preserving their structure and order. The model service remains responsible for
question semantics and its own supported limits; the gateway does not silently
truncate inputs or invent cross-provider validation rules.

Reuse the current JSON parsing, 32 MiB body limit, and routing checks. Use the
existing buffered-only handler flag, as Embeddings does, rather than adding a
System One-specific streaming error policy. The native parameter table does not
inject chat streaming, reasoning, or engine parameters. Reject E2EE headers
using the existing unsupported-endpoint behavior, as Responses does.

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

Preserve native `model`, `answers`, and `usage` instead of converting them to
chat choices or explanations. Keep the existing processing in each topology:

- Direct-upstream mode uses buffered passthrough. Do not add canonicalization,
  identity rewriting, or gateway cost injection to this path.
- Middleware mode uses its existing JSON decoding, identity rewriting, pricing,
  and canonicalization. Add a System One branch retaining top-level `model`,
  `answers`, and `usage`, and usage fields `input_tokens`, `output_tokens`, and
  gateway-owned `cost`. Preserve `answers` as content without nested allowlists,
  including answer `type`, `noul`, legends, probabilities, and confidence. This
  follows the existing Responses treatment of structured output content.

The gateway does not recompute weighted scores, enforce an argmax choice, check
probability sums, or reinterpret a model's decision. Those are model semantics,
not additional gateway acceptance gates. Malformed JSON and transport failures
follow the existing response and error paths; add no endpoint-specific response
buffering limit or retry mechanism. Any future generic hardening belongs in the
shared pipeline rather than a System One exception.

Keep existing identity handling in each topology. In middleware mode,
`rewrite_identity` continues to report the requested public model ID; do not add
a special resolved-version override. This differs from
[TypeSafe's alias-resolution behavior](https://docs.typesafe.ai/models) and must
be documented and tested instead of claiming full alias compatibility.

Pass confidence through unchanged. It is a
[distribution-derived statistic](https://docs.typesafe.ai/confidence), not a
guarantee of correctness or an interchangeable maximum probability.

In middleware mode, reuse the existing usage resolver, which already recognizes
`input_tokens` and `output_tokens`, pricing, and the gateway-owned `usage.cost`
extension. Keep actual counters even when the output price is zero. Post-consult
receives raw provider usage before client cost injection, just as on other
endpoints. Direct-upstream responses retain the provider's usage unchanged.

### Errors and retries

Keep the current error behavior in each topology. Middleware uses the existing
OpenAI-style gateway error envelope, upstream-status mapping, sanitization,
ordered candidate handling, and failure accounting. Direct-upstream mode
preserves upstream error bodies and statuses; it does not gain middleware
sanitization. Gateway validation and ACI refusals keep their existing handling.

Do not add System One-specific status codes, validation errors, retry rules, or
idempotency semantics. The current middleware failover set is
`401`, `402`, `403`, `404`, `429`, `500`, `502`, `503`, and `504`; request errors
such as `400`, `405`, and `422` do not advance to another candidate. In
particular, upstream `529` is not a failover or capacity-retry signal: middleware
maps it to `502`, while direct-upstream mode preserves `529`. Retain and document
this compatibility difference rather than adding a special retry policy.

## Integration with this gateway

### HTTP and middleware

Add a path constant, handler, and `Endpoint::SystemOne`. Reuse the common body
limit, caller identity, ACI constraint extraction, middleware authorization,
forwarding, metering, and receipt finalization. Avoid a second HTTP proxy stack.

Add endpoint entries to the existing shaping and response dispatch. The current
default in
[`handlers.rs`](../src/http/app/handlers.rs) selects Chat Completions for an
unrecognized middleware endpoint. The chat allowlists in
[`response_transform.rs`](../src/middleware/response_transform.rs) would remove
`answers` and native usage counters. Select the native field table and
canonicalizer without changing the common lifecycle or identity policy.
Direct-upstream mode continues through its existing forwarding path rather
than gaining a second processing stack.

### Control-plane contract

Keep the existing [`/consult/pre`](control-plane-contract.md#post-consultpre)
request and candidate schemas. No new pre-consult `endpoint` field or
`format: "systemone"` value is needed.

Use the existing `format: "openai"` candidate and its endpoint-specific parameter
table, as for native Embeddings and Responses. This does not turn the payload
into Chat Completions: the existing dispatch selects the body shape by both
format and endpoint. An Anthropic-only route cannot shape this request and uses
the existing unsupported-format handling.

Declare native support using the existing `supportedEndpoints` field with
`/v1/systemone`. This metadata is not automatically enforced for every endpoint:
the reviewed [`build_candidates`](../src/middleware/request_transform.rs) loop
currently calls `supports_endpoint` only for native Responses selection. Add a
System One-only check using the same helper in that loop, skipping candidates
that omit `/v1/systemone`. Then use the existing format shaping and skip-on-error
behavior, preserving candidate order. Only a declared native route that can
shape this endpoint is eligible.

If all candidates are excluded, use the existing
[empty-candidate failure path](../src/aggregator/service/middleware.rs); if
shaping fails for all candidates, retain the existing transform error path.
Neither case forwards a request, chooses a default route, or bridges to chat.
Do not rely on an incompatible upstream returning `404`, since `400`, `405`,
and `422` would stop failover. This is an endpoint predicate, not a new routing
framework or control-plane protocol. Chat reasoning and engine transforms
remain limited to the endpoint branches that already use them.

Feature extraction returns no chat features for System One, following the
existing Embeddings/Responses pattern. Do not invent state-derived token
estimates or prefix hashes. The existing post-consult report already contains
`endpoint`; retain its usage and failure accounting and send no state, questions,
or answers to the control plane.

Add configured model entries to the reference control plane using its existing
fields, not a new configuration vocabulary. Do not add an `UpstreamProvider`
variant for Jev or Kev: a model protocol is not a new attestation mechanism.

### Catalog and SDK boundary

Use the existing catalog configuration and TEE-only filtering. Configure a
decision model only after establishing that its route implements the native
endpoint; a generic decision-model label is not enough. No new catalog
validation or discovery mechanism is proposed.

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
| Native endpoint | Route, endpoint dispatch, native request and response fields, and credential-free mock-upstream tests through the existing direct and middleware paths. |
| Middleware | Consume existing capability metadata in the candidate loop: skip an unsupported candidate, serve through a later eligible candidate, and fail without forwarding when none qualify. Keep pre-consult unchanged and reuse failure handling, pricing, identity handling, and content-free reports. |
| ACI integration | One verified success with matching receipt hashes and one fail-closed rejection on the new endpoint. Reuse the existing shared suites for binding, session, TEE-only, and ownership coverage rather than duplicating them. |
| SDK compatibility | Mixed-primitive decision calls using a pinned official TypeSafe Python SDK against a local fixture, including structured criteria and common errors. Record the tested version and public-model identity policy; discovery and unverified SDKs remain separate. |

Contract tests should cover preservation of question IDs, option order,
structured criteria, and native answer fields; topology-specific usage and error
handling, including `529`; buffered-only operation; and unsupported E2EE. Verify
that middleware preserves complete answer content while filtering top-level and
usage fields, and that direct-upstream mode retains the original body and
status. Forward model-provided scores and probabilities without recomputing
them. Existing chat, Messages, Responses, embeddings, catalogs, and receipt
tests must remain
unchanged in behavior. Model evaluation and provider admission are separate
tasks, not additional endpoint logic.

When implementation lands, update the living
[HTTP API reference](api-reference.md),
[control-plane contract](control-plane-contract.md), and any changed
[configuration fields](configuration-reference.md) in the same change. Move the
feature out of the [roadmap](roadmap.md) only when code, tests, and those
references agree. A confidential deployment additionally needs the applicable
[provider review](providers/audit-criteria.md) and verification notes.
