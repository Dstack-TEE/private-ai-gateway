# Frontend / Middleware / Backend Framework

Date: 2026-05-21 UTC.
Status: implementation in progress. The no-middleware path carries an internal
request context, the backend accepts out-of-band target routes, and runtime
HTTP-over-UDS middleware mode exercises frontend -> middleware -> internal
backend.

Private AI Gateway should be an ACI shell around optional routing logic. One
gateway process owns the downstream ACI frontend and the verified-provider
backend. A middleware slot, when configured, runs ordinary plaintext
OpenAI-compatible HTTP logic between them over Unix domain sockets.

```text
user
  -> gateway frontend
  -> optional middleware
  -> gateway backend
  -> verified upstream provider
```

## Target Shape

| Part | Trust-critical responsibilities |
| --- | --- |
| Frontend | Public OpenAI-compatible and ACI endpoints, downstream E2EE, public-header sanitization, request context creation, user-facing hashes, receipt signing. |
| Middleware | Optional open-source plaintext HTTP business logic: billing, policy, cache-aware routing, request rewrite, response post-processing, and model catalog shaping. It does not perform ACI or provider verification. |
| Backend | Configured target validation, provider model rewrite, provider verification leases, upstream TLS binding, provider E2EE sealing, backend-authored receipt facts. |

Frontend and backend live in the same gateway process. Middleware is optional.
With no middleware, frontend calls backend directly and current behavior must
stay unchanged.

The backend is local-only: either in-process or bound to a Unix socket. It
accepts configured route ids, never arbitrary upstream URLs.

## Request Flow

1. User sends a request to the public frontend.
2. Frontend strips external `X-Private-AI-Gateway-*` headers, validates the
   request, decrypts downstream E2EE if present, and creates `request_id`.
3. If middleware is disabled, frontend calls backend directly with:

   ```text
   user_model = body.model
   target_route_id = body.model
   effective_body = decrypted user body
   ```

4. If middleware is enabled, frontend sends plaintext HTTP over UDS to
   middleware with `request_id` and the frontend-observed user model.
   Middleware may rewrite the body and select a configured target route id.
5. Backend validates `request_id` and target route id, refreshes the provider
   lease if needed, rewrites to the upstream model id, seals the request if the
   provider transport requires it, and forwards.
6. Response data returns through backend, optional middleware, then frontend.
   Frontend performs downstream E2EE response encryption if needed and signs
   the receipt after the response stream completes.

For streaming, the frontend may return `x-receipt-id` early. The signed receipt
is complete only after stream finalization.

`/v1/models` follows the same ownership split. With middleware enabled,
frontend passes the request through middleware so the middleware can expose its
user-facing catalog. Without middleware, frontend returns the backend catalog
from the configured routes.

## Model Names

Middleware must not rewrite the user model just to pick a provider. The design
keeps three names separate:

| Name | Owner | Example | Use |
| --- | --- | --- | --- |
| User model | User / frontend | `glm51` | User request body and downstream E2EE AAD. |
| Target route id | Middleware / backend config | `glm51:tinfoil` | Backend route selection. |
| Upstream model id | Backend/provider | `glm-5-1` | Provider-facing request body. |

ACI v2 request and response AAD use the frontend-observed user model. They do
not use the middleware-selected target route or upstream model id.

## Request Context and Receipts

Frontend creates an in-memory context keyed by `request_id`:

```text
request_id
user_model
endpoint
e2ee state
receipt journal
frontend-observed request hashes
backend-authored events
```

Middleware receives `request_id`, but it has no authority over verification
facts. Backend looks up the context and writes provider facts directly through
shared gateway state. Unknown or expired request ids are rejected.

Receipt events should distinguish requested routing from verified forwarding:

| Event | Author | Meaning |
| --- | --- | --- |
| `request.received` | Frontend | User-facing request observed by the ACI service. |
| `middleware.forwarded` | Frontend or middleware adapter | Effective body after middleware rewrite. |
| `route.selected` | Backend | Backend accepted a configured target route. |
| `upstream.verified` | Backend | Provider/session verification and channel binding. |
| `upstream.forwarded` | Backend | Provider-facing request after backend rewrite/seal. |
| `response.received` | Backend | Provider response before middleware post-processing. |
| `response.returned` | Frontend | Final user-facing response and wire hashes. |

Backend facts must come from backend observations, not middleware-provided
headers or claims.

## Security Rules

- Only frontend exposes public endpoints.
- Frontend strips external internal-use headers before creating trusted context.
- Backend is local-only and rejects unknown `request_id`.
- Backend accepts configured target route ids only.
- Provider forwarding requires the configured verification/binding to succeed.
- First-version middleware is open-source and part of the attested workload. It
  can see plaintext prompts by design.

## Config Sketch

Disabled mode stays the default. If no middleware socket path is configured,
the frontend calls the backend directly in-process.

```text
# no middleware variables required
```

Middleware mode is enabled by one Unix socket path. The gateway also starts an
internal backend Unix socket for the middleware to call:

```text
PRIVATE_AI_GATEWAY_MIDDLEWARE_UDS_PATH=/run/private-ai-gateway/middleware.sock
PRIVATE_AI_GATEWAY_BACKEND_UDS_PATH=/run/private-ai-gateway/backend.sock
```

The pending request-context TTL is 300 seconds. A middleware must call
`POST /internal/forward` before the context expires.

Middleware developers should implement the wire contract in
[middleware-integration.md](middleware-integration.md).

## Implementation Tasks

1. Introduce internal request context keyed by `request_id`. Done.
2. Split the current path into frontend prep, backend verification/forwarding,
   and frontend finalization. Done for the current UDS middleware path,
   including streaming response finalization.
3. Keep middleware-disabled mode as the default and preserve current tests. Done.
4. Add local backend entrypoint guarded by request context lookup. Done.
5. Add UDS middleware mode with a fixture middleware test. Done.
6. Add tests for forged internal headers, target route validation, request
   context expiry, and E2EE AAD using the original user model. Done for the
   current UDS middleware path.
7. Split receipt events into `middleware.forwarded`, `route.selected`, and
   final `request.forwarded`. Done for middleware-selected routes.
8. Finalize middleware-mode receipts in the frontend after middleware returns
   the final user-visible response. Done.
9. Preserve streaming across backend, middleware, and frontend. Done.
10. Pass user headers to middleware for middleware-owned auth. Done.
11. Pass middleware-generated OpenAI-compatible responses through downstream
    E2EE. Done.
12. Add deployment compose wiring for a concrete middleware container.
13. Publish the middleware developer integration guide. Done.
