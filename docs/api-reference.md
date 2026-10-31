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
| `X-ACI-Keyset-Digest` | digest of the keyset current when the response headers were produced; the exchange's receipt cites the same digest. |

The router permits cross-origin browser requests. Inference handlers read JSON
bodies under a 32 MiB limit. A larger body receives `413` in the surface's JSON
error envelope. The Anthropic envelope includes the request ID; the OpenAI
envelope does not.

Buffered upstream responses default to a 64 MiB operator limit, configurable with
[`max_buffered_response_bytes`](configuration-reference.md#buffered-response-memory), including upstream error
bodies and the decompressed output of Chutes E2EE responses. An advertised
response length over this limit is rejected before reading the body; responses
without a known length are stopped when they exceed the limit. Oversized
responses return `502 upstream_error`. This limit does not apply to responses
forwarded as streams; failures after headers are sent follow the streaming
error behavior below.

## Inference endpoints

| Method and path | Surface | Streaming | ACI E2EE v2 | Notes |
| --- | --- | --- | --- | --- |
| `POST /v1/chat/completions` | OpenAI Chat Completions | Yes | Yes | Primary chat endpoint. |
| `POST /v1/completions` | OpenAI legacy Completions | Yes | Yes | Encrypts `prompt` in E2EE mode. |
| `POST /v1/audio/transcriptions` | OpenAI multipart transcription | No | No; any `x-e2ee-*` header causes `400 e2ee_unsupported_endpoint` before parsing or admission | JSON or verbose JSON; middleware requires explicit endpoint opt-in. |
| `POST /v1/images/generations` | OpenAI Image Generations | No | No; E2EE headers cause `400 e2ee_unsupported_endpoint` before parsing or admission | Buffered JSON with inline `data[].b64_json` only. Middleware requires explicit `supportedEndpoints` opt-in. |
| `POST /v1/embeddings` | OpenAI Embeddings | No | Yes | The gateway forces a client-supplied `stream: true` back to buffered mode. |
| `POST /v1/responses` | OpenAI Responses create | Yes | No | E2EE headers return `400 e2ee_unsupported_endpoint`. In middleware mode, a candidate that lists `/v1/responses` in `supportedEndpoints` receives the request unchanged; any other candidate receives a converted chat completion. Dedicated System One candidates receive neither. See the [candidate fields](control-plane-contract.md#post-consultpre). |
| `POST /v1/messages` | Anthropic Messages | Yes | No; E2EE headers return `400 e2ee_unsupported_endpoint` | Middleware can translate between Anthropic and OpenAI provider formats. |
| `POST /v1/systemone` | System One structured decisions | No | No; E2EE headers return `400 e2ee_unsupported_endpoint` | Accepts native `model`, `state`, and `questions` fields and returns native `model`, `answers`, and `usage` fields. Middleware forwards only to candidates that list `/v1/systemone` in `supportedEndpoints`, and sends no other endpoint to them; it never converts the request to chat. |

System One requests and responses are buffered JSON. Middleware preserves in-band
`error` using the shared public error sanitizer, removing upstream metadata.
The gateway passes native `state`, `questions`, and nested `answers` values through
without interpreting question types, scores, probabilities, or confidence. In
middleware mode, `usage` keeps `input_tokens`, `output_tokens`, and the
gateway-added `cost` when present. The endpoint has no E2EE v2 field contract.

Image generation requires string `model` and `prompt` (at most 32,000 Unicode
characters). Request up to `n` images (`n` is an integer from 1 through 10,
default 1); providers may return fewer, and image charges use the returned
count. `n` is forwarded unchanged without retries to fill the count. `size`, `quality`,
`background`, `output_format`, and `moderation` are strings;
`output_compression` is an integer from 0 through 100. Provider-specific values
for the string options are validated upstream. `response_format` must be absent
or `"b64_json"`; the gateway always forwards `"b64_json"`. `user`,
`safety_identifier`, and `style` are accepted as strings and removed before
forwarding; user identifiers stay private to the gateway. `provider` uses the
existing routing and ACI constraints. Any other field returns `400` with
`error.code: "unknown_parameter"` and `error.param` naming that field. `seed`
is not supported. Non-integer and out-of-range `n` values return
`400 invalid_request_error` with `code: "invalid_parameter"` and `param: "n"`.

`response_format: "url"`, any `stream` or `partial_images` field, invalid types,
and out-of-range integers return `400`. Images allow 16 concurrent generations
and buffered response bodies per gateway process, with immediate
`429 rate_limit_error` when full and no queue. Admission happens after the capped
upload is read and validated; in middleware mode, it also follows control-plane
admission and route selection. The response body and any pending write buffers
retain the slot until completion, error, or disconnect releases them. There is
no heartbeat while waiting for an image. The buffered upstream response limit
uses the configured operator policy (64 MiB by default). A 2xx response with an
empty `data` array, more images than requested, or any item missing a string
`b64_json` becomes a sanitized
`502 upstream_error` in both routing modes, before receipt hashing and image
billing.

The image response schema contains only top-level `created`, `data`, `usage`,
`size`, `quality`, `output_format`, and `background`; each item retains only
`b64_json` and `revised_prompt`. Usage retains `input_tokens`, `output_tokens`,
`total_tokens`, `input_tokens_details`, `output_tokens_details`, and the gateway
`cost`. `created` is a required integer; a missing or invalid upstream value
is replaced with the current Unix time in seconds. Dimensions and format
fields are strings, and `revised_prompt` is a string or null. Token counts are non-negative
integers; token detail objects retain only non-negative integer `text_tokens`,
`image_tokens`, and `cached_tokens`. `cost` is numeric. Malformed optional
fields are dropped, including structured values under allowed names. Every
successful response has a complete `usage` object with integer `input_tokens`,
`output_tokens`, and `total_tokens`, plus `input_tokens_details` containing
integer `text_tokens` and `image_tokens`. Missing or invalid required counters
are filled with `0`. Optional `output_tokens_details` is omitted when absent;
when present, its missing or invalid `text_tokens` and `image_tokens` are also
filled with `0`. When the provider reports no token usage, all five
counters are `0`; image charges still use the returned image count. Middleware
adds `usage.cost` when pricing applies, including for providers billed per image.
URLs, provider IDs, model IDs, and other fields are removed. Image
receipts have `chat_id: null`; retrieve them using `X-Receipt-Id`. Image pricing
follows the [control-plane pricing contract](control-plane-contract.md#image-generation).

### Audio transcription

Images and audio require an `Authorization` header in both direct and middleware
modes. Requests missing it receive `401` before parsing, buffering, or admission,
using the bounded discard described in the configuration reference. In middleware
mode, credentials continue through the existing control-plane authorization.

Send `multipart/form-data` to `/v1/audio/transcriptions`. Required fields are
`model` and exactly one nonempty `file` (up to 25 MiB), with filename extension
`flac`, `mp3`, `mp4`, `mpeg`, `mpga`, `m4a`, `ogg`, `wav`, or `webm`.
The file part must include a filename with a supported extension. In the Node
SDK, use `toFile(buf, 'audio.wav')`; `toFile(buf)` sends `unknown_file` and
returns `400` because that filename has no extension.
No audio decoding or MIME sniffing occurs. Optional fields are `language` (2–3
lowercase ASCII letters), `prompt` (up to 4096 Unicode characters), `temperature`
(0–1), `response_format` (`json`, the default, or `verbose_json`), and `user`
(accepted but removed upstream). `timestamp_granularities[]` (or the spelling
without brackets) accepts `word` and/or `segment`, once each, only with
`verbose_json`; mixing spellings is rejected. `stream=false` is accepted and
removed; `stream=true` or any other value returns `400`. `provider` form fields,
`chunking_strategy`, `include[]`, speaker fields, and all other fields return
`400 unknown_parameter`. Duplicate scalar fields, invalid UTF-8, ambiguous
parts, and malformed multipart return `400`.

Limits: 64 KiB aggregate text, 70-byte boundary, 8192-byte part headers,
255-byte filename, and a whole-body cap of 25 MiB + 128 KiB. Uploads share a
402 MiB byte budget (16 times the whole-body limit) and a 60-second whole-upload deadline
(`408 upload_timeout`). A 25 MiB file needs about 3.5 Mbps to finish within
that deadline. Each received chunk reserves only its actual bytes;
`Content-Length` is used only for early 413 rejection. Connections with no
buffered data reserve no upload capacity. Budget exhaustion returns
`429 rate_limit_exceeded` with `Retry-After: 1`. Size failures (`413`) and upload
timeouts (`408`) close the connection. A separate 16-slot transcription inference
admission follows validation and, in middleware mode, successful control-plane
authorization and route selection. Upload accounting remains held until that
handoff. Response bodies and pending write buffers own inference admission until
completion or disconnect. There are no queue or whitespace heartbeats.

Multipart routing policy uses `X-ACI-Provider: {"aci_verified":true}` with the
same object schema as JSON `provider`. Duplicate headers, values over 8192 bytes,
non-ASCII bytes, malformed/non-object JSON, invalid pins, and unknown `aci_`
members return `400`.
The header is consumed; it never reaches the upstream. JSON endpoints reject
this carrier. The attestation report advertises support as
`service_capabilities.supported_multipart_constraint_endpoints: ["/v1/audio/transcriptions"]`.
Constrained clients must obtain this capability over their accepted pinned
channel before uploading. Receipt body hashes do not commit to this header;
clients retain their effective policy and check the cited session against it.
TEE-only host policy remains independent.

The gateway forwards the requested response format. NEAR audio automatically
uses verbose output internally to obtain duration while preserving the caller's
output format. It requests word timestamps only when the caller asks. `json` returns
`text`, `usage`, and optional well-formed `languages: [{code: string}]`; verbose output may
also include `language`, `duration`,
`segments`, `words`, and `task`. Nested segment, word, and usage
objects are sanitized by allowlist. Upstream filenames become `audio.<ext>`.
Verbose `duration` always contains the validated raw duration. `language` uses
the upstream string, then the requested language, then an empty string for
unknown. Segment and word items must contain all SDK-required fields with valid
types; incomplete items are dropped, and empty timestamp arrays are omitted.
Audio upstream responses have a separate 8 MiB cap, independent of
`max_buffered_response_bytes`, including error bodies (`502` on
overflow).

A successful response requires string `text` and a positive numeric duration:
`usage.seconds` with `usage.type == "duration"`, otherwise top-level `duration`.
An invalid preferred duration never falls back. Zero, negative, missing,
nonfinite, and durations above `4294967295` seconds become `502`; the transcript
is withheld and no audio usage is billed. The forwarded receipt is retained.
Audio pricing is USD per second of input audio, billed per started second;
see the [audio pricing contract](control-plane-contract.md#audio-transcription).
Client-visible usage is always `{"type":"duration","seconds":3}` with optional
gateway-priced `cost`; upstream `usage.cost` is discarded. `seconds` is the
billable integer (`2.965` becomes `3`).
Provider token counters are discarded; audio pricing and post reports use
duration only. Verbose `duration` retains the validated raw value.
Receipts have null `chat_id`; fetch by
`X-Receipt-Id`. `request.received` hashes raw client multipart bytes and
`request.forwarded` hashes the exact gateway-encoded bytes sent upstream.

For audio/image header rejections, the refusal is decided before parsing or
admission. The gateway then discards unread bytes without buffering, up to the
endpoint body limit, with a 5-second total deadline, 1-second idle deadline,
and 32 separate discard slots. This preserves the 400 JSON through streaming
reverse proxies for uploads within those bounds. Exhausted slots, excess bytes,
transport errors, or a deadline cause immediate connection closure; upload
capacity and inference permits are never held by the discard.

### Inference responses

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

In middleware mode, an unconstrained, non-E2EE stream can be committed as HTTP
`200` before the upstream answers, and that response omits `X-Receipt-Id`; see
[Streaming keepalives and early commit](configuration-reference.md#streaming-keepalives-and-early-commit).

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
| `412` | `session_not_accepted` | None of the pinned `aci_session_ids` is current. Sessions rotate on re-verification; re-list `/v1/aci/sessions` and retry once. |
| `503` | `upstream_verification_failed` | No eligible route passed verification with an enforceable binding. |

In middleware mode, a request on a
[TEE-only hostname](configuration-reference.md#tee-only-hostnames) requires ACI
verification even when it sends `aci_verified: false`.

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

### Receipt events in middleware mode

Besides the ACI events, a receipt for a middleware request carries three
extension events, which verifiers that do not know them ignore (ACI §7.4):

| Event | Records |
| --- | --- |
| `middleware.forwarded` | Hash of the body shaped for the selected route |
| `route.selected` | The selected route's `target_route_id` |
| `response.received` | Hash of the upstream response body before response transforms |

## Model catalogs

| Method and path | Direct mode | Middleware mode |
| --- | --- | --- |
| `GET /v1/models` | Returns the configured model-router catalog. | Relays `/models` and the query string to the control plane. |
| `GET /v1/models/{subpath}` | `404` | Relays `/models/{subpath}` and the query string to the control plane. |
| `GET /v1/embeddings/models` | `404` | Relays `/embeddings/models` and the query string to the control plane. |

On a [TEE-only hostname](configuration-reference.md#tee-only-hostnames), the
gateway relays `tee=true` in place of any client-supplied `tee` parameter.

## Canonical ACI endpoints

| Method and path | Authentication | Response |
| --- | --- | --- |
| `GET /v1/aci/attestation?nonce=<value>` | Public | Bare ACI attestation report. `nonce` must be exactly 64 lowercase hex characters; any other value returns `400`. The nonce is bound into `report_data`; omitting it binds JSON `null`. |
| `GET /v1/aci/receipts/{id}` | Original bearer token for owned receipts | Bare signed receipt. `{id}` accepts `receipt_id` or an upstream chat ID. |
| `GET /v1/aci/sessions/{session_id}` | Public | Full immutable attested-session record, including evidence data when recorded. |
| `GET /v1/aci/sessions?upstream_name=<name>&model=<id>` | Public | Newest-first list of sessions accepted by the pin gate right now. The broad list omits evidence data and keeps its digest. |

Use a fresh, unpredictable attestation nonce for each trust decision. When
[downstream TLS bindings](configuration-reference.md#downstream-tls-binding)
are configured, fetch the report through the same public hostname used for
inference; the report handlers select the binding by `Host`.

The list is exactly the sessions the pin gate accepts now for the queried
channels; it never verifies afresh, so an empty list means no current verified
state. Query `?model=<public alias>`; see [Preflight survey](attested-session-system.md#preflight-survey)
for direct versus middleware routing. `expires_at` is an upper bound; superseded
sessions stay retrievable by id. On `412`, re-list and retry once.

A session's evidence object has two fields. `data` is a
`data:<content-type>;base64,<bytes>` URI holding the exact bytes the verifier
received. `digest` is `sha256:<hex>` over the decoded bytes, not over a parsed
JSON value. When a verifier keeps several upstream responses, `data` is one
`multipart/mixed` URI whose parts carry each response's content type, source
URL, and body, and `digest` covers the whole decoded payload.

The wrapper carries no provider meaning of its own. What the bytes prove is
defined by the provider's verifier, documented on its
[verification page](providers/README.md). The gateway enforces only the
verifier's result and channel binding.

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
| `PUT /v1/admin/upstreams` | Admin bearer token | Validates, atomically writes, and activates a replacement JSON array. Wakes the background verification supervisor; the response does not wait for verification. |

[Admin API](configuration-reference.md#admin-api) defines the admin response,
validation, and authentication statuses.

## Middleware failover behavior

The control plane returns ordered route candidates. Middleware mode tries the
next candidate when the provider answers `401`, `402`, `403`, `404`, `429`,
`500`, `502`, `503`, `504`, or `520`–`527` (a proxy in front of the provider
could not reach the node behind it), or any `5xx` whose body carries the
provider's out-of-capacity marker. A `404` means that provider does not serve
the model, so another provider may. A `400`, a `422`, or a failed fetch of a
caller-supplied image URL ends the request, because every candidate would
receive the same input.

When no candidate succeeds and some answered with `429` or the capacity marker,
the gateway waits 2000 ms plus up to 2000 ms of random jitter and retries those
candidates once. When a rejection carried a longer `Retry-After` (in seconds),
the gateway waits that long instead, up to ten seconds. It skips this retry
after ten seconds of forwarding. The receipt records the route that served the
response. Usage reports carry each attempt to the control plane.

## Error handling

Middleware errors use the envelope of the requesting surface: OpenAI on the
OpenAI paths and Anthropic on `/v1/messages`.

| Condition | Response |
| --- | --- |
| Control plane denies the request | The status and message the control plane chose. |
| Control plane denies with a rate limit | `429` with `X-RateLimit-Limit`, `X-RateLimit-Remaining`, `X-RateLimit-Reset`, and `Retry-After`. |
| Control plane is unavailable or returns an invalid decision | `503 control plane unavailable`. |
| Control plane allows but names no usable route | `404 model_not_found`. |
| Catalog request cannot reach the control plane | `502 control plane unavailable`. |

[Denials](control-plane-contract.md#denials) defines how a consult response maps
to these statuses.

The following errors use the OpenAI envelope on every path, including
`/v1/messages`: E2EE header and decryption errors, `X-Upstream-Verification`,
invalid JSON, invalid `provider.aci_*` fields, and errors the gateway generates
in direct mode other than the `504` for an upstream timeout.

The gateway passes actionable upstream client errors through in its own
envelope, maps upstream authentication failures to gateway errors, and maps a
recognized provider capacity-exhaustion response to `429`.

Streaming failures that occur after response headers are sent, including after
an early commit, are represented inside the stream where the protocol permits
it. Receipt or E2EE finalization errors end the body without forcing a TCP
reset. The statuses middleware reports to the control plane for these failures
are listed under [`POST /consult/post`](control-plane-contract.md#post-consultpost).
