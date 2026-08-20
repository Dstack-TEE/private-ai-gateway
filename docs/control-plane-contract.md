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
5. Calls `POST /consult/post` for usage and failure records.

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
messages, tool arguments, files, or media to the control plane:

| Field | Contract |
| --- | --- |
| `estimatedPromptTokens` | Low-biased heuristic for one inference context. It is not a tokenizer result or a guaranteed bound. For batched legacy completions, this is the largest item rather than the sum. A control plane may use it to order candidates but must not use it to remove the final candidate. |
| `hasTools` | `true` when the request contains current `tools` or legacy `functions`. |
| `inputModalities` | Deduplicated, stably ordered values from `text`, `image`, `file`, `audio`, and `video`. |
| `reasoning` | Client intent: `enabled`, `disabled`, or `unspecified`. Response-visibility controls do not change this value. |
| `responseFormat` | `text`, `json_object`, or `json_schema`. A missing response format becomes `text`. |
| `prefixHash` | Optional 32-character lowercase hex cache-affinity key for the canonical first 4 KiB of a conversation. It is present only when that prefix fills the 4 KiB cap. The digest is HMAC-SHA256 when `middleware.prefix_hash_secret` is set and plain SHA-256 otherwise. It supports prefix matching; it does not prove prompt contents or request authenticity. |

Treat these fields as routing hints. The gateway deliberately uses closed enums,
counts, booleans, and an optional one-way digest so the external control plane
does not receive inference content.

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
  "virtualKeyId": 7,
  "spendMode": "regular",
  "userTier": "pro"
}
```

| Response field | Required | Contract |
| --- | --- | --- |
| `allow` | Yes | Boolean decision. |
| `pricing` | No | Opaque pricing object interpreted by the current cost calculator. Use numeric strings or numbers for supported per-token fields. |
| `candidates` | Required for an allowed inference | Ordered route candidates. An empty or missing list produces a no-route error. |
| `candidates[].routeId` | Yes | `<upstream name>:<public model ID>` matching the active gateway upstream config. |
| `candidates[].format` | Yes | `openai` or `anthropic`. Selects request and response transformation. |
| `candidates[].engine` | No | `sglang` or `vllm` for engine-specific shaping. Omit for managed APIs. |
| `candidates[].reasoningFormat` | No | Upstream reasoning dialect: `reasoning_effort`, `reasoning`, `chat_template_thinking`, `chat_template_enable_thinking`, or `thinking_type`. `thinking_type` writes DeepSeek's `thinking.type` switch and uses `reasoning_effort` for the level. When omitted, managed routes use `reasoning` and self-hosted engines use `reasoning_effort`. |
| `candidates[].reasoningPolicy` | No | Deployment policy interpreted by the gateway. It can contain `override`, `default`, and a token `threshold`; reasoning configs use `effort`, `maxTokens`, or `enabled` as defined by the current middleware types. |
| `userId` | No | Opaque integer copied to post-consult reports. |
| `virtualKeyId` | No | Opaque integer copied to post-consult reports. |
| `spendMode` | No | `regular`, `subscription`, or `subscription_overflow`. |
| `userTier` | No | Passed upstream as `x-user-tier`. The value `basic` disables the delayed capacity retry. |
| `rateLimit` | No | Used on a denied `429`; object fields are `limit` and Unix-seconds `resetAt`. |

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

Any non-200 response, timeout, transport error, or invalid JSON from
`/consult/pre` becomes a `503 control plane unavailable` denial. The gateway
does not forward the inference request.

The gateway reports a denial to `POST /consult/post` when the response contains
`userId`, or when its status is `429` or `5xx`. An identity-free `400`, `401`,
`402`, or `403` remains tracing-only so unauthenticated traffic cannot flood the
usage pipeline. An allowed response with no usable candidates becomes a
reported `404`. These reports have `selectedRouteId: null` and
`errorSource: "control"`.

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
  "virtualKeyId": 7
}
```

The stable fields are:

| Field | Presence | Meaning |
| --- | --- | --- |
| `requestId` | Always | Groups every attempt and summary record for one client request. |
| `endpoint` | Always | Public inference path. |
| `status` | Always | Status attributed to this attempt or summary. |
| `durationMs` | Always | Elapsed request time when the report was produced. |
| `selectedRouteId` | Always, nullable | Route for an attempt. `null` identifies a request-level summary. |
| `requestModel` | Always | Public model requested by the client, or an empty string when absent. |
| `prefixHash` | Optional | Echo of the pre-consult request feature. Use it as a cache-affinity key, not as proof of request content. |
| `usage` | Always, nullable | Raw provider usage before client cost injection. |
| `pricing` | Always, nullable | Pricing returned by the pre-consult. |
| `ttftMs` | Optional | Time to first token for a streaming attempt. |
| `isStreaming` | Optional | Whether the report covers a streaming path. |
| `attemptIndex` | Optional | Zero-based attempt order. Use this with `requestId` when ingesting retries. |
| `spendMode`, `userId`, `virtualKeyId` | Optional | Values copied from the pre-consult. |
| `errorSource` | Optional | `control`, `upstream`, or `gateway`. |
| `errorMessage` | Optional | Error text for a failed record. Treat it as sensitive operational data. |

A request can produce multiple reports. Count attempts only when
`selectedRouteId` is non-null. A record with a null route and a non-empty
`errorSource` is a request-level failure, such as a reported consult denial, a
no-route result, or the summary after the candidate chain failed.

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
