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

The router permits cross-origin browser requests. Axum limits request bodies to
32 MiB. JSON inference requests that exceed the limit receive `413` before the
inference handler runs.

## Inference endpoints

| Method and path | Surface | Streaming | ACI E2EE v2 | Notes |
| --- | --- | --- | --- | --- |
| `POST /v1/chat/completions` | OpenAI Chat Completions | Yes | Yes | Primary chat endpoint. |
| `POST /v1/completions` | OpenAI legacy Completions | Yes | Yes | Encrypts `prompt` in E2EE mode. |
| `POST /v1/embeddings` | OpenAI Embeddings | No | Yes | The gateway forces a client-supplied `stream: true` back to buffered mode. |
| `POST /v1/responses` | OpenAI Responses create | Yes | No | E2EE headers return `400 e2ee_unsupported_endpoint`. |
| `POST /v1/messages` | Anthropic Messages | Yes | No supported field profile | Middleware can translate between Anthropic and OpenAI provider formats. |

Every completed provider-backed response includes:

| Header | Meaning |
| --- | --- |
| `X-Receipt-Id` | Preferred identifier for the signed receipt. |
| `X-E2EE-Applied` | `true` when the gateway encrypted the response under ACI or legacy E2EE, otherwise `false` on normal completed inference responses. |
| `X-E2EE-Version` | E2EE wire version when encryption was applied. |
| `X-E2EE-Algo` | Selected E2EE key algorithm when encryption was applied. |

For streaming calls, the gateway sends `X-Receipt-Id` before the stream is
complete and stores the receipt after the stream finalizer observes the terminal
body. An upstream non-2xx returned before streaming begins is buffered and does
not receive a receipt.

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
`aci_verified: false` returns `400`.

The constraint causes the gateway to reject before forwarding when:

- the selected route is not classified as a TEE route,
- the route has no successful verifier result,
- the current session ID is not in the supplied allowlist, or
- the backend cannot enforce the verified binding.

Direct mode removes `aci_verified` and `aci_session_ids` before forwarding the
provider block. Middleware mode sends the original provider block to the
control plane and builds a provider-specific upstream body from the returned
route candidate.

`X-Upstream-Verification` is retired. Requests that send it receive `400` with
an instruction to use `provider.aci_verified`.

### Receipt ownership

If the inference request includes `Authorization: Bearer <token>`, the gateway
stores a SHA-256 digest of that token as the receipt owner. Receipt lookup then
requires the same bearer token:

- a missing token returns `401`,
- a different token returns `403`, and
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
| `GET /v1/aci/attestation?nonce=<value>` | Public | Bare ACI attestation report. The URL-decoded nonce is bound into `report_data`; omission binds JSON `null`. |
| `GET /v1/aci/receipts/{id}` | Original bearer token for owned receipts | Bare signed receipt. `{id}` accepts `receipt_id` or an upstream chat ID. |
| `GET /v1/aci/sessions/{session_id}` | Public | Full immutable attested-session record, including evidence data when recorded. |
| `GET /v1/aci/sessions?upstream_name=<name>&model=<id>` | Public | Newest-first session list. The broad list omits evidence data and keeps its digest. |

Use a fresh, unpredictable attestation nonce for each trust decision. Fetch the
report through the same public hostname used for inference when downstream TLS
bindings are configured.

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
next candidate after provider-specific authentication or credit failures
(`401`, `402`, `403`), capacity signals (`429`), and selected upstream failures
(`500`, `502`, `503`, `504`). Request errors such as `400`, `404`, and `422` are
terminal because another candidate would receive the same invalid request.

After all candidates report capacity, a non-`basic` user tier can receive one
delayed retry of the capacity-failed candidates. The retry waits about two
seconds and is skipped after ten seconds of elapsed forwarding time. The
receipt records the route that served the response. Per-attempt usage reports
carry the full failover chain to the control plane.

## Error handling

The gateway keeps OpenAI and Anthropic error envelopes separate. It passes
actionable upstream client errors through the appropriate surface, maps most
upstream authentication failures to gateway errors, and maps a recognized
provider capacity-exhaustion response to `429`.

Streaming failures that occur after response headers are sent are represented
inside the stream where the protocol permits it. Receipt or E2EE finalization
errors end the body without forcing a TCP reset.
