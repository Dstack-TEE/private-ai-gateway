# Middleware Integration Guide

This guide is for developers building a routing or business-logic middleware
inside Private AI Gateway. Middleware is optional. It runs between the public
ACI frontend and the provider-verifying backend, and it talks ordinary
OpenAI-compatible HTTP over Unix domain sockets.

The current middleware contract supports request routing, request rewriting,
response post-processing, streaming responses, and model catalog shaping.

## When To Use Middleware

Use middleware when you need logic that should see plaintext prompts but should
not own ACI or provider verification:

- choosing a target provider for a request
- exposing a tenant-specific `/v1/models` catalog
- rewriting OpenAI-compatible requests before provider selection
- applying policy before the request is sent to a verified provider
- collecting business metrics inside the same attested workload

Do not put provider attestation, TLS/SPKI checks, Chutes E2EE sealing, receipt
signing, or ACI E2EE in middleware. The gateway backend owns provider trust.
The gateway frontend owns downstream ACI.

## Runtime Wiring

The middleware router is available through the Rust HTTP router helpers. The
checked-in gateway binary and static config do not expose middleware socket
fields. Current deployments run no-middleware mode unless they embed the
middleware router wiring in a custom binary.

In this mode the gateway has three HTTP surfaces:

| Surface | Who Calls It | Purpose |
| --- | --- | --- |
| Public gateway bind, for example `127.0.0.1:8086` | Downstream users and SDKs | ACI/OpenAI-compatible frontend. |
| Middleware UDS, for example `/run/private-ai-gateway/executor.sock` | Gateway frontend | Plaintext routing and business logic. |
| Internal backend UDS, for example `/run/private-ai-gateway/backend.sock` | Middleware only | Verified provider forwarding. |

Run middleware in the same attested deployment as the gateway. The socket paths
must be visible to both containers or processes through the deployment's local
filesystem mounts.

## Request Flow

1. The user calls the public gateway endpoint.
2. The gateway frontend validates JSON, terminates downstream E2EE if present,
   strips unsupported empty `tool_calls`, records the frontend-observed model,
   and creates a one-use `request_id`.
3. The gateway frontend calls middleware with plaintext JSON and internal
   routing headers.
4. Middleware chooses one configured target route id and calls the internal
   backend.
5. The backend validates the `request_id`, consumes it, verifies the selected
   provider lease, rewrites the body to the upstream model id, sends the
   request, and records backend-owned receipt facts.
6. Middleware returns the backend response to the gateway frontend. The
   frontend finalizes the receipt against that final response and returns it to
   the user.

The `request_id` expires after 300 seconds and is consumed by the first
`POST /internal/forward` call. Middleware must not call multiple target routes
with the same `request_id`.

## Endpoints Middleware Must Implement

### `GET /v1/models`

The gateway forwards public `/v1/models` to middleware when middleware mode is
enabled. Return an OpenAI-compatible model list with user-facing model ids.

Example response:

```json
{
  "object": "list",
  "data": [
    {
      "id": "fast-private-model",
      "object": "model",
      "owned_by": "private-ai-gateway"
    }
  ]
}
```

The ids in this response are user model names. They do not have to equal backend
target route ids.

### `POST /v1/chat/completions`

The gateway calls the same path on middleware for chat completions.

Middleware receives:

| Header | Meaning |
| --- | --- |
| `content-type: application/json` | Body is JSON after gateway frontend processing. |
| `x-private-ai-gateway-request-id` | One-use context key for the internal backend call. |
| `x-private-ai-gateway-user-model` | The frontend-observed request `model`, when it was a string. |
| `authorization` and user headers | Forwarded for middleware-owned authentication and routing. |

The body is the plaintext OpenAI-compatible request after downstream E2EE
termination. If the user sent ACI E2EE, middleware sees decrypted content by
design. The gateway forwards user headers except hop-by-hop HTTP headers,
gateway-owned `x-private-ai-gateway-*` / `x-aci-*` headers, and downstream E2EE
protocol headers that no longer match the decrypted body.

Middleware must call:

```http
POST http://private-ai-gateway/internal/forward
x-private-ai-gateway-request-id: <request id from frontend>
x-private-ai-gateway-targets: <ordered, comma-separated route ids>
content-type: application/json

<possibly rewritten OpenAI-compatible JSON body>
```

`x-private-ai-gateway-targets` is an ordered, comma-separated list of backend
route ids (highest priority first). The backend tries each in order until one
commits, performing request-level failover internally (verification/binding
failure, transport error, or a retryable provider HTTP error before the first
response byte advances to the next candidate). A single route id is just a
one-element list. For routes loaded from the gateway upstream config, each
route id is:

```text
<upstream name>:<public model id in upstream config>
```

For example, this upstream config creates target route
`tinfoil:kimi-k2`:

```json
[
  {
    "name": "tinfoil",
    "provider": "tinfoil",
    "base_url": "https://inference.tinfoil.sh",
    "models": {
      "kimi-k2": "kimi-k2-6"
    },
    "bearer_token": "<secret>"
  }
]
```

The request body sent to `/internal/forward` may keep the user-facing model
name. The backend selects the committed candidate from
`x-private-ai-gateway-targets` and rewrites `body.model` to the configured
upstream model id before provider forwarding.

Mixed-format candidates (e.g. an OpenAI-compatible route and a native Anthropic
`/v1/messages` route in one failover list) use the envelope body form instead of
a single shared body:

```json
{
  "candidates": [
    { "target": "anthropic:claude", "body": { "model": "claude", "messages": [] } },
    { "target": "openrouter:claude", "body": { "model": "claude", "messages": [] } }
  ]
}
```

The backend forwards each candidate's own body verbatim to that upstream's
configured `path`. When the envelope is used, its target order is
authoritative; if `x-private-ai-gateway-targets` is also present it must match.

The backend returns route attribution on the `/internal/forward` response for
the middleware to record metrics and bill the actually-served deployment:
`x-private-ai-gateway-selected-route`, `x-private-ai-gateway-attempts`, and
`x-private-ai-gateway-session-id` (the attested session id, when the committed
route established one). These are internal-hop headers; the frontend strips any
leaked `x-private-ai-gateway-*` before the user sees the response.

### `POST /v1/completions`

The legacy completions endpoint follows the same middleware contract as chat:
receive plaintext JSON at `/v1/completions`, then call `/internal/forward`
with the same `request_id` and a configured target route id.

## Return The Final Response

Middleware may relay or post-process the backend response. The gateway frontend
signs the receipt only after middleware returns, so `response.returned` binds
the final user-visible body.

At minimum, relay these headers when present:

| Header | Why It Matters |
| --- | --- |
| `content-type` | Preserves JSON or SSE response type. |
| `x-receipt-id` | Lets users fetch the signed ACI receipt; the frontend also overwrites it from the finalized receipt. |

The public frontend strips gateway-owned response headers from middleware
responses before finalization. Middleware should not mint `x-receipt-id`,
`x-e2ee-*`, `x-aci-*`, or `x-private-ai-gateway-*` headers.

The backend records `response.received` for the provider response before
middleware post-processing. The frontend records `response.returned` for the
final cleartext and wire bytes after middleware returns. If middleware changes
the response, the receipt includes `transparency.response_modified`.

For downstream E2EE, middleware still sees and returns plaintext. The frontend
encrypts the final response after middleware returns and then records both the
final cleartext hash and encrypted wire hash.

Middleware may reject a request before calling `/internal/forward`, but that
response will not have an ACI receipt because no provider inference occurred.
If the original user request used downstream E2EE, the frontend applies the
same response E2EE to middleware-generated OpenAI-compatible responses before
returning them.

## Minimal Middleware Example

This Python example exposes one user-facing model and always routes it to
`tinfoil:kimi-k2`.

```python
from fastapi import FastAPI, Header, Request, Response
import httpx

BACKEND = "http://private-ai-gateway"
BACKEND_UDS = "/run/private-ai-gateway/backend.sock"
TARGET_ROUTE = "tinfoil:kimi-k2"

app = FastAPI()


@app.get("/v1/models")
def models():
    return {
        "object": "list",
        "data": [
            {
                "id": "fast-private-model",
                "object": "model",
                "owned_by": "private-ai-gateway",
            }
        ],
    }


async def forward(body: bytes, request_id: str) -> Response:
    transport = httpx.AsyncHTTPTransport(uds=BACKEND_UDS)
    async with httpx.AsyncClient(
        base_url=BACKEND,
        transport=transport,
        timeout=None,
    ) as client:
        backend = await client.post(
            "/internal/forward",
            headers={
                "content-type": "application/json",
                "x-private-ai-gateway-request-id": request_id,
                "x-private-ai-gateway-targets": TARGET_ROUTE,
            },
            content=body,
        )

    headers = {}
    for name in (
        "content-type",
        "x-receipt-id",
        "x-e2ee-applied",
        "x-e2ee-version",
        "x-e2ee-algo",
    ):
        if name in backend.headers:
            headers[name] = backend.headers[name]

    return Response(
        content=backend.content,
        status_code=backend.status_code,
        headers=headers,
    )


@app.post("/v1/chat/completions")
async def chat_completions(
    request: Request,
    request_id: str = Header(alias="x-private-ai-gateway-request-id"),
):
    body = await request.body()
    return await forward(body, request_id)


@app.post("/v1/completions")
async def completions(
    request: Request,
    request_id: str = Header(alias="x-private-ai-gateway-request-id"),
):
    body = await request.body()
    return await forward(body, request_id)
```

Run it locally:

```bash
uvicorn middleware:app --uds /run/private-ai-gateway/executor.sock
```

The current static gateway config has no middleware fields. Use the integration
tests as the executable reference for middleware router wiring.

Then call the public gateway as usual:

```bash
curl -sS http://127.0.0.1:8086/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{
    "model": "fast-private-model",
    "messages": [{"role": "user", "content": "Say hello in one sentence."}]
  }'
```

## Receipts In Middleware Mode

For middleware-selected routes, receipts include:

| Event | Meaning |
| --- | --- |
| `request.received` | User-facing request observed by the gateway frontend. |
| `middleware.forwarded` | Body middleware sent to the internal backend. |
| `route.selected` | Target route accepted by the backend. |
| `request.forwarded` | Provider-facing body after backend model rewrite. |
| `upstream.verified` | Provider verification result and channel bindings. |
| `response.received` | Provider response before middleware post-processing. |
| `response.returned` | Final user-visible response cleartext and wire hashes. |

`route.selected`, `request.forwarded`, `upstream.verified`, and
`response.received` are backend facts. Middleware cannot assert them through
headers.

## Error Handling

Internal backend errors use the gateway's OpenAI-style error shape:

```json
{
  "error": {
    "message": "unknown or expired request id",
    "type": "invalid_internal_request",
    "code": null,
    "param": null
  }
}
```

Common errors:

| Status | Type | Cause |
| --- | --- | --- |
| `400` | `invalid_internal_request` | Missing, empty, unknown, expired, or reused `request_id` / target header. |
| `400` | `invalid_request_error` | Middleware sent invalid JSON to `/internal/forward`. |
| `400` | `model_routing_error` | Target route id is not configured. |
| `503` | `upstream_verification_failed` | Provider verification or channel binding failed. |
| `500` | `internal_error` | Gateway bug or unexpected service error. |

On backend errors, relay the backend status and body to the public caller.

## Current Limitations

- Pending request context is in memory. A gateway restart invalidates all
  in-flight middleware requests.
