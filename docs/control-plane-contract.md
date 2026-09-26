# Control-plane HTTP contract

This reference is for teams implementing the external control plane used by the
gateway's optional in-process middleware. The control plane decides who may call
a model and which configured route candidates the gateway should try. It does
not create attestation facts or bypass backend verification.

Set `middleware.control_url` to the control-plane base URL. The gateway appends
the paths in this document. If `middleware.control_token` is configured, every
control-plane request includes `Authorization: Bearer <token>`.

## Request flow

For an inference request, the gateway:

1. Parses and, when needed, decrypts the client body.
2. Calls `POST /consult/pre` before any provider request.
3. Shapes one upstream request per returned candidate.
4. Tries the candidates in order and finalizes the client response.
5. Calls `POST /consult/post` for admitted attempts and reportable request
   outcomes, including failures and client cancellation.

The pre-consult fails closed. The post-consult is best effort because the client
response may already have been served.

## `POST /consult/pre`

The request uses camelCase:

```json
{
  "apiKeyHash": "3f2c...",
  "model": "public-model-id",
  "provider": {
    "only": ["provider-a"],
    "aci_verified": true
  },
  "request": {
    "estimatedPromptTokens": 128,
    "hasTools": true,
    "inputModalities": ["image", "text"],
    "reasoning": "enabled",
    "responseFormat": "json_schema",
    "prefixHash": "0123456789abcdef0123456789abcdef"
  },
  "tee": true
}
```

| Field | Required | Meaning |
| --- | --- | --- |
| `apiKeyHash` | No | Lowercase SHA-256 hex of the client's bearer token. Omitted for anonymous requests. |
| `model` | No at the wire level | Public model ID from the client body. A control plane should reject a missing model for model inference. |
| `provider` | No | Original client provider-routing object, forwarded without schema reduction. The control plane should validate all policy fields it supports. |
| `request` | No | Content-derived routing features. Omitted when `middleware.send_request_features` is false or the gateway does not recognize the endpoint body. |
| `tee` | No | `true` when the request arrived on a hostname in `middleware.tee_only_domains`. The control plane should deny a non-TEE model, normally with `404`. |

### Request features

The gateway derives the optional `request` block without sending the prompt,
messages, tool arguments, files, or media to the control plane. For the full
list of what the control plane receives, see
[What leaves the request path](attested-confidential-inference.md#what-leaves-the-request-path).

| Field | Contract |
| --- | --- |
| `estimatedPromptTokens` | Low-biased heuristic for one inference context. It is not a tokenizer result or a guaranteed bound. For batched legacy completions, this is the largest item rather than the sum. A control plane may use it to order candidates but must not use it to remove the final candidate. |
| `hasTools` | `true` when the request contains current `tools` or legacy `functions`. |
| `inputModalities` | Deduplicated, stably ordered values from `text`, `image`, `file`, `audio`, and `video`. |
| `reasoning` | Client intent: `enabled`, `disabled`, or `unspecified`. Response-visibility controls do not change this value. |
| `responseFormat` | `text`, `json_object`, or `json_schema`. A missing response format becomes `text`. |
| `prefixHash` | Optional 32-character lowercase hex cache-affinity key for the canonical first 4 KiB of a conversation. It is present only when that prefix fills the 4 KiB cap. With `middleware.prefix_hash_secret` set, it is HMAC-SHA256 and reveals only whether two prefixes are equal. Without the secret, it is plain SHA-256, so the control plane can also confirm a prefix it already knows by hashing it. It does not prove prompt contents or request authenticity. |

Treat these fields as routing hints. They are counts, booleans, closed enums,
and one digest. They carry no prompt text, but they do describe the request,
and `prefixHash` links requests that share a prefix.

An allow response:

```json
{
  "allow": true,
  "pricing": {
    "inputCostPerToken": "0.000001",
    "outputCostPerToken": "0.000002"
  },
  "candidates": [
    {
      "routeId": "provider-a:public-model-id",
      "format": "openai",
      "engine": "vllm",
      "reasoningFormat": "reasoning_effort"
    }
  ],
  "userId": 42,
  "organizationId": 11,
  "workspaceId": 13,
  "virtualKeyId": 7,
  "spendMode": "regular",
  "userTier": "pro"
}
```

| Response field | Required | Contract |
| --- | --- | --- |
| `allow` | Yes | Boolean decision. |
| `pricing` | No | Per-token prices as numeric strings or numbers: `inputCostPerToken`, `outputCostPerToken`, `cacheReadCostPerToken`, and `cacheCreationCostPerToken`. A missing input or output price counts as zero; a missing cache price uses the input price. The object is copied to post-consult reports. |
| `candidates` | Required for an allowed inference | Ordered route candidates. An empty or missing list produces a no-route error. |
| `candidates[].routeId` | Yes | `<upstream name>:<public model ID>` matching the active gateway upstream config. |
| `candidates[].format` | Yes | `openai` or `anthropic`. Selects request and response transformation. |
| `candidates[].engine` | No | `sglang` or `vllm` for engine-specific shaping. Omit for managed APIs. |
| `candidates[].reasoningFormat` | No | Upstream reasoning dialect: `reasoning_effort`, `reasoning`, `chat_template_thinking`, `chat_template_enable_thinking`, or `thinking_type`. `thinking_type` writes DeepSeek's `thinking.type` switch and uses `reasoning_effort` for the level. When omitted, managed routes use `reasoning` and self-hosted engines use `reasoning_effort`. |
| `candidates[].reasoningPolicy` | No | Deployment reasoning policy applied by the gateway. See [Reasoning policy](#reasoning-policy). |
| `candidates[].supportedEndpoints` | No | Paths the upstream serves natively. A `/v1/responses` request goes unchanged to a candidate that lists `/v1/responses`; any other candidate receives a chat completion, and the gateway converts the response back to the Responses shape. |
| `userId`, `organizationId`, `workspaceId` | No for anonymous traffic; otherwise all three required | Positive integer tenant identity and resource scope, copied to post-consult reports. Partial groups, zero, and negative values make the consult response invalid. |
| `virtualKeyId` | No | Opaque integer copied to post-consult reports. |
| `spendMode` | No | `regular`, `subscription`, or `subscription_overflow`. |
| `userTier` | No | Sent to every upstream attempt as the `x-user-tier` header. |
| `rateLimit` | No | Used on a denied `429`; object fields are `limit` and Unix-seconds `resetAt`. |

### Reasoning policy

`reasoningPolicy` applies to chat completion bodies, including `/v1/responses`
requests converted to chat:

| Key | Contract |
| --- | --- |
| `override` | Reasoning config for structured output: a request with `response_format` and no `tools`. It replaces the caller's reasoning. |
| `default` | Used in place of `override` when `override` is absent. With neither, the caller's reasoning stays. |
| `threshold` | For a request with `tools`: when `max_completion_tokens` or `max_tokens` is at or below this value, reasoning is set to effort `none`. |
| `omitMaxTokens` | When `true` on an `openai` candidate, the gateway removes `max_tokens` and `max_completion_tokens` from a request that asks for `json_object` or `json_schema` output and has no tools or functions. |

A reasoning config has `effort` (`max`, `xhigh`, `high`, `medium`, `low`,
`minimal`, or `none`), `maxTokens`, and `enabled`. An unknown key in
`reasoningPolicy` or in a reasoning config makes the whole pre-consult response
invalid, and the gateway denies the request with `503`.

### Denials

A denial response can return the client-facing status and message:

```json
{
  "allow": false,
  "status": 401,
  "message": "Invalid API key"
}
```

If `status` is absent, the gateway uses `403`. A denied `429` may include:

```json
{
  "allow": false,
  "status": 429,
  "message": "Rate limit exceeded",
  "rateLimit": {
    "limit": 100,
    "resetAt": 1786406400
  }
}
```

The gateway turns that block into `Retry-After` and `X-RateLimit-*` response
headers.

Any non-200 response, timeout, transport error, or body from `/consult/pre`
that does not parse under the rules above becomes a
`503 control plane unavailable` denial. The gateway
does not forward the inference request.

The gateway reports a denial to `POST /consult/post` when the response carries
a tenant identity or its status is `429` or `5xx`. Any other denial without an
identity produces no report and no log line, so unauthenticated traffic cannot
flood the usage pipeline. An allowed response with no usable candidates becomes
a reported `404 model_not_found`. These reports have `selectedRouteId: null`
and `errorSource: "control"`.

## `POST /consult/post`

The gateway emits post-consult records for completed attempts and selected
gateway failures. The JSON shape is camelCase:

```json
{
  "requestId": "req_0123...",
  "endpoint": "/v1/chat/completions",
  "status": 200,
  "durationMs": 812,
  "ttftMs": 94,
  "isStreaming": true,
  "attemptIndex": 0,
  "selectedRouteId": "provider-a:public-model-id",
  "requestModel": "public-model-id",
  "prefixHash": "0123456789abcdef0123456789abcdef",
  "usage": {
    "prompt_tokens": 12,
    "completion_tokens": 8,
    "total_tokens": 20
  },
  "pricing": {
    "inputCostPerToken": "0.000001",
    "outputCostPerToken": "0.000002"
  },
  "spendMode": "regular",
  "userId": 42,
  "organizationId": 11,
  "workspaceId": 13,
  "virtualKeyId": 7
}
```

The stable fields are:

| Field | Presence | Meaning |
| --- | --- | --- |
| `requestId` | Always | Groups every attempt and summary record for one client request. |
| `endpoint` | Always | Public inference path. |
| `status` | Always | Status attributed to this attempt or summary. It can differ from the already-committed downstream HTTP status for a streaming failure. |
| `durationMs` | Always | Elapsed request time when the report was produced. |
| `selectedRouteId` | Always, nullable | Route for an attempt. `null` identifies a request-level summary. |
| `requestModel` | Always | Public model requested by the client, or an empty string when absent. |
| `prefixHash` | Optional | Echo of the pre-consult request feature. Use it as a cache-affinity key, not as proof of request content. |
| `usage` | Always, nullable | Raw provider usage before client cost injection. |
| `pricing` | Always, nullable | Pricing returned by the pre-consult. |
| `ttftMs` | Optional | Time to first token for a streaming attempt. |
| `isStreaming` | Optional | Whether the report covers a streaming path. |
| `attemptIndex` | Optional | Zero-based attempt order. Use this with `requestId` when ingesting retries. |
| `spendMode`, `userId`, `organizationId`, `workspaceId`, `virtualKeyId` | Optional | Values copied from the pre-consult; the three tenant identity fields travel together. |
| `errorSource` | Optional | `control`, `upstream`, or `gateway`. |
| `errorMessage` | Optional | One failure class from the closed list below, never provider or request text. |

A request can produce multiple reports. Count attempts only when
`selectedRouteId` is non-null. A record with a null route and a non-empty
`errorSource` is a request-level failure, such as a reported consult denial, a
no-route result, or the summary after the candidate chain failed.

Streaming and cancellation do not erase accounting:

- a client disconnect before the first upstream byte produces status `499`,
  names the route in flight, and omits `ttftMs`;
- a gateway-enforced connect or read deadline produces status `504` for the
  attempt and request outcome;
- attempts already completed during failover are reported even if the client
  disconnects while a later candidate is in flight; and
- an unconstrained stream can already be HTTP `200` because the gateway emitted
  a keepalive before the upstream answered. If forwarding later fails, the
  in-band error and post-consult record carry the real failure status.

`errorMessage` takes one of these values:

| Origin | `errorMessage` values |
| --- | --- |
| Caller | `client_disconnected` |
| Control plane | `control_denied`, `control_unavailable`, `model_not_found` |
| Upstream response | `upstream_http_error`, `upstream_quota_exhausted`, `upstream_capacity`, `upstream_image_fetch_failed`, `upstream_malformed_response`, `upstream_response_failed` |
| Upstream connection | `upstream_timeout`, `upstream_transport`, `upstream_channel_binding_mismatch`, `upstream_verification_failed` |
| Stream | `stream_inband_error`, `stream_truncated`, `stream_line_overflow` |
| Gateway | `request_shaping_failed`, `no_eligible_attested_route`, `e2ee_failed`, `receipt_failed`, `downstream_finalizer_failed`, `internal_error` |

These values still reveal operational failure classes; apply access and
retention controls to usage reports.

Requests the gateway rejects before the pre-consult produce no report. These
include malformed JSON, E2EE setup failures, an oversized body, and an invalid
`provider.aci_*` field.

The gateway may retry a broken pooled connection once. A control plane must
ingest post-consult reports idempotently. At minimum, deduplicate by the stable
request and attempt identity used by your billing model.

The gateway ignores a post-consult timeout or transport failure after logging
it. A failed usage report does not change the already selected client response.

## Catalog endpoints

The gateway removes `/v1` from public catalog paths and relays the remaining
path and query string:

| Public gateway request | Control-plane request |
| --- | --- |
| `GET /v1/models` | `GET /models` |
| `GET /v1/models/providers/acme?zdr=true` | `GET /models/providers/acme?zdr=true` |
| `GET /v1/embeddings/models` | `GET /embeddings/models` |

The control plane owns the response schema and status. The gateway relays the
body and status with `content-type: application/json`. A transport failure
becomes `502 control plane unavailable`.

For a TEE-only hostname, the gateway forces `tee=true` in the relayed query.
The control plane must implement that filter if the deployment relies on the
catalog to hide non-TEE models. Backend serving still requires a verified TEE
route for those hostnames.

## Reference implementation

[`examples/control-plane`](../examples/control-plane/README.md) contains a
small config-backed server for local integration tests. It implements the core
`/models` and consult routes. It is not a production billing, TEE-catalog, or
policy service.
