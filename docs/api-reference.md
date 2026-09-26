# HTTP API reference

This reference is for client and control-plane implementers integrating with
the current gateway binary. The ACI wire-format definitions are normative in
the [ACI specification](../spec/aci.md).

## Base behavior

The gateway has one HTTP listener, configured by `bind`. It does not terminate
TLS. A production deployment normally places a TLS endpoint in front of the
listener and forwards the original `Host` header.

All responses, including errors, carry:

| Header | Value |
| --- | --- |
| `X-ACI-Version` | `aci/1` |
| `X-ACI-Keyset-Digest` | digest of the active workload keyset |

The router permits cross-origin browser requests. Inference handlers read JSON
bodies under a 32 MiB limit. A larger body receives `413` in the surface's JSON
error envelope. The Anthropic envelope includes the request ID; the OpenAI
envelope does not.

## Inference endpoints

| Method and path | Surface | Streaming | ACI E2EE v2 | Notes |
| --- | --- | --- | --- | --- |
| `POST /v1/chat/completions` | OpenAI Chat Completions | Yes | Yes | Primary chat endpoint. |
| `POST /v1/completions` | OpenAI legacy Completions | Yes | Yes | Encrypts `prompt` in E2EE mode. |
| `POST /v1/embeddings` | OpenAI Embeddings | No | Yes | The gateway forces a client-supplied `stream: true` back to buffered mode. |
| `POST /v1/responses` | OpenAI Responses create | Yes | No | E2EE headers return `400 e2ee_unsupported_endpoint`. In middleware mode, a candidate that lists `/v1/responses` in `supportedEndpoints` receives the request unchanged; any other candidate receives a converted chat completion. See the [candidate fields](control-plane-contract.md#post-consultpre). |
| `POST /v1/messages` | Anthropic Messages | Yes | No; E2EE headers return `400 e2ee_unsupported_endpoint` | Middleware can translate between Anthropic and OpenAI provider formats. |

The normal provider-backed response path adds:

| Header | Meaning |
| --- | --- |
| `X-Receipt-Id` | Preferred identifier for the signed receipt. |
| `X-E2EE-Applied` | `true` when the gateway encrypted the response under ACI or legacy E2EE, otherwise `false` on normal completed inference responses. |
| `X-E2EE-Version` | E2EE wire version when encryption was applied. |
| `X-E2EE-Algo` | Selected E2EE key algorithm when encryption was applied. |

For a normal streaming call, the gateway sends `X-Receipt-Id` before the stream
is complete and stores the receipt after the finalizer observes the terminal
body. An upstream non-2xx returned before streaming begins is buffered and does
not receive a receipt.

Middleware has one exception. An unconstrained, non-E2EE stream can be committed
as HTTP `200` after `middleware.sse_keepalive_ms` elapses without upstream
response headers. The gateway emits `: PROCESSING` comments while it waits. It
cannot name a receipt when it commits, so the response omits `X-Receipt-Id`. If
the upstream later succeeds and the stream finalizes, fetch the receipt by the
response `id`; if forwarding fails after the early commit, no receipt is
drafted and the real failure arrives as an in-band error event. Requests with
`provider.aci_verified` or pinned session IDs are never committed early, so
ACI-constrained clients always receive the receipt header.

### Require ACI verification

The request body may contain a gateway-owned provider constraint:

```json
{
  "provider": {
    "aci_verified": true,
    "aci_session_ids": ["0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"]
  }
}
```

`aci_verified` must be a boolean. `aci_session_ids` must be a non-empty array of
bare 64-character lowercase hexadecimal IDs; duplicates are collapsed.
Supplying session IDs implies `aci_verified: true`; combining session IDs with
`aci_verified: false` returns `400`. Any other `provider` field starting with
`aci_` also returns `400`.

The constraint causes the gateway to reject before forwarding when:

- the selected route is not classified as a TEE route,
- the route has no successful verifier result,
- the current session ID is not in the supplied allowlist, or
- the backend cannot enforce the verified binding.

A rejection carries `X-Receipt-Id` for a signed refusal receipt:

| Status | `error.type` | Meaning |
| --- | --- | --- |
| `412` | `session_not_accepted` | None of the pinned `aci_session_ids` is current. Fetch the session list and pin again. |
| `503` | `upstream_verification_failed` | No eligible route passed verification with an enforceable binding. |

In middleware mode, a request on a hostname listed in
`middleware.tee_only_domains` requires ACI verification even when it sends
`aci_verified: false`. When that list is non-empty, a request with a missing or
malformed `Host` is treated the same way. See
[TEE-only hostnames](configuration-reference.md#tee-only-hostnames).

Direct mode removes `aci_verified` and `aci_session_ids` before forwarding the
provider block. Middleware mode sends the original provider block to the
control plane and builds a provider-specific upstream body from the returned
route candidate.

`X-Upstream-Verification` is not supported. Requests that send it receive `400` with
an instruction to use `provider.aci_verified`.

### Receipt ownership

If the inference request includes `Authorization: Bearer <token>`, the gateway
stores a SHA-256 digest of that token as the receipt owner. Receipt lookup then
requires the same bearer token:

- a missing token returns `401`,
- a different token returns `404` so the lookup does not reveal another
  tenant's receipt, and
- an inference request without a bearer token creates a public receipt.

The incoming bearer token is used for receipt ownership and middleware API-key
hashing. Provider credentials come from the upstream config.

## Model catalogs

| Method and path | Direct mode | Middleware mode |
| --- | --- | --- |
| `GET /v1/models` | Returns the configured model-router catalog. | Relays `/models` and the query string to the control plane. |
| `GET /v1/models/{subpath}` | `404` | Relays `/models/{subpath}` and the query string to the control plane. |
| `GET /v1/embeddings/models` | `404` | Relays `/embeddings/models` and the query string to the control plane. |

On a host listed in `middleware.tee_only_domains`, the gateway removes any
client-supplied `tee` query parameter and relays `tee=true`.

## Canonical ACI endpoints

| Method and path | Authentication | Response |
| --- | --- | --- |
| `GET /v1/aci/attestation?nonce=<value>` | Public | Bare ACI attestation report. `nonce` must be exactly 64 lowercase hex characters; any other value returns `400`. The nonce is bound into `report_data`; omitting it binds JSON `null`. |
| `GET /v1/aci/receipts/{id}` | Original bearer token for owned receipts | Bare signed receipt. `{id}` accepts `receipt_id` or an upstream chat ID. |
| `GET /v1/aci/sessions/{session_id}` | Public | Full immutable attested-session record, including evidence data when recorded. |
| `GET /v1/aci/sessions?upstream_name=<name>&model=<id>` | Public | Newest-first session list. The broad list omits evidence data and keeps its digest. |

Use a fresh, unpredictable attestation nonce for each trust decision. Fetch the
report through the same public hostname used for inference when downstream TLS
bindings are configured. On such a deployment, a `Host` that matches no
configured domain receives `404` instead of a gateway report from both
`/v1/aci/attestation` and `/v1/attestation/report`.

A session's evidence object has two fields. `data` is a
`data:<content-type>;base64,<bytes>` URI holding the exact bytes the verifier
received. `digest` is `sha256:<hex>` over the decoded bytes, not over a parsed
JSON value. When a verifier keeps several upstream responses, `data` is one
`multipart/mixed` URI whose parts carry each response's content type, source
URL, and body, and `digest` covers the whole decoded payload.

## Legacy compatibility endpoints

| Method and path | Behavior |
| --- | --- |
| `GET /v1/attestation/report` | Returns the gateway report with dstack-vllm-proxy compatibility fields. Query parameters include `nonce`, `signing_algo`, `version`, and `model`. |
| `GET /v1/signature/{id}` | Returns the legacy signature wrapper with the canonical ACI receipt nested under `receipt`. |

`GET /v1/attestation/report?model=<id>` may add upstream GPU evidence for
supported providers. Chutes returns its own multi-instance legacy report. New
ACI verifiers should use the canonical endpoints and follow the receipt's
session reference.

## Operations endpoints

| Method and path | Authentication | Purpose |
| --- | --- | --- |
| `GET /` | Public | Returns `api_version` and `workload_keyset_digest`. |
| `GET /health` | Public | Liveness only. Returns `{"status":"ok"}`. |
| `GET /v1/metrics` | Public | Prometheus text generated by the gateway. |
| `GET /v1/admin/upstreams` | Admin bearer token | Returns the active config digest and a redacted upstream list. |
| `PUT /v1/admin/upstreams` | Admin bearer token | Validates, atomically writes, and activates a replacement JSON array. Starts background verification prewarm after the response. |

If `admin_token` is absent from the static config, all admin routes return
`404`. With an admin token configured, a missing token returns `401` and a wrong
token returns `403`.

## Middleware failover behavior

The control plane returns ordered route candidates. Middleware mode tries the
next candidate when the provider answers `401`, `402`, `403`, `404`, `429`,
`500`, `502`, `503`, or `504`, or any `5xx` whose body carries the provider's
out-of-capacity marker. A `404` means that provider does not serve the model,
so another provider may. A `400`, a `422`, or a failed fetch of a
caller-supplied image URL ends the request, because every candidate would
receive the same input.

When no candidate succeeds and some answered with `429` or the capacity marker,
the gateway waits 2000 ms plus up to 2000 ms of random jitter and retries those
candidates once. It skips this retry after ten seconds of forwarding. The
receipt records the route that served the response. Usage reports carry each
attempt to the control plane.

## Error handling

Middleware errors use the envelope of the requesting surface: OpenAI on the
OpenAI paths and Anthropic on `/v1/messages`.

| Condition | Response |
| --- | --- |
| Control plane denies the request | The denial's `status` and `message`. Default status `403`. |
| Control plane denies with `429` and a `rateLimit` block | `429` with `X-RateLimit-Limit`, `X-RateLimit-Remaining`, `X-RateLimit-Reset`, and `Retry-After`. |
| Pre-consult fails, times out, or returns a non-200 status or invalid JSON | `503 control plane unavailable`. |
| Control plane allows but returns no candidates | `404 model_not_found`. |
| Catalog request cannot reach the control plane | `502 control plane unavailable`. |

The following errors use the OpenAI envelope on every path, including
`/v1/messages`: E2EE header and decryption errors, `X-Upstream-Verification`,
invalid JSON, invalid `provider.aci_*` fields, and errors the gateway generates
in direct mode other than the `504` for an upstream timeout.

The gateway passes actionable upstream client errors through in its own
envelope, maps upstream authentication failures to gateway errors, and maps a
recognized provider capacity-exhaustion response to `429`.

Streaming failures that occur after response headers are sent are represented
inside the stream where the protocol permits it. This includes failures after
an early HTTP `200`; the stream's error event carries the status the response
would otherwise have used, and middleware reports that real status to the
control plane. A client disconnect before the first upstream byte is reported
as `499`, and a gateway-enforced connect or read deadline is reported as `504`.
Receipt or E2EE finalization errors end the body without forcing a TCP reset.
