# Configuration Reference

Private AI Gateway uses one read-only static config file and one writable state
directory. Operators must choose the config file with
`PRIVATE_AI_GATEWAY_CONFIG_PATH` and put gateway policy in that file.

## Runtime Files

| Item | Owner | Runtime path |
| --- | --- | --- |
| Static gateway config | Deployment | Required. Selected by `PRIVATE_AI_GATEWAY_CONFIG_PATH`. |
| Upstream seed config | Deployment | Selected by `upstream_config_seed_path` in the static gateway config. |
| Active upstream config | Gateway | `<state_dir>/upstreams.json` |
| Attested-session log | Gateway | `<state_dir>/sessions.jsonl` |

Operators configure `state_dir`, not the individual writable files inside it.
The gateway creates `state_dir` on startup, seeds `upstreams.json` from the
read-only upstream seed only when the active file is missing or empty, and
updates `upstreams.json` through `PUT /v1/admin/upstreams` or an authenticated
`upstream_pull` source.

Unknown fields in the static gateway config are rejected at startup.

## Minimal Config

This is the smallest practical container config.

```json
{
  "bind": "0.0.0.0:8086",
  "state_dir": "/var/lib/private-ai-gateway",
  "upstream_config_seed_path": "/etc/private-ai-gateway/upstreams.seed.json",
  "upstream_pull": {
    "url": "https://control.example/api/admin/gateway-upstreams/config",
    "token": "<dedicated-machine-pull-token>",
    "refresh_seconds": 300,
    "request_timeout_seconds": 90
  },
  "admin_token": "<long-random-admin-token>",
  "dstack_endpoint": "unix:/var/run/dstack.sock"
}
```

## Config Fields

| Field | Default | Meaning |
| --- | --- | --- |
| `bind` | `127.0.0.1:8086` | Public HTTP listener address. Use `0.0.0.0:8086` in containers that expose the gateway port. |
| `state_dir` | `/var/lib/private-ai-gateway` | Gateway-owned writable state directory. The active upstream config and attested-session log are derived from this directory. |
| `upstream_config_seed_path` | unset | Read-only JSON seed copied to `<state_dir>/upstreams.json` only when the active upstream config is missing or empty. |
| `upstream_pull` | unset | Optional authenticated HTTPS source for the complete runtime upstream config. See [Upstream Pull](#upstream-pull). |
| `admin_token` | unset | Bearer token for `GET` and `PUT /v1/admin/upstreams`. When unset, the admin API is not exposed. |
| `dstack_endpoint` | dstack SDK default | dstack SDK endpoint, such as `unix:/var/run/dstack.sock`. |
| `enable_e2ee` | `true` | Advertise and terminate the [E2EE v2 compatibility extension](../spec/e2ee-v2.md). Set to `false` only for an explicit TLS-only deployment; the attestation then reports `supported_e2ee_versions: []` and v2 requests fail with `e2ee_invalid_version`. |
| `middleware` | unset | Optional middleware section. When present, the gateway consults a control plane to route and authorize each request and applies request/response transforms; when unset it serves directly. See [Middleware](#middleware). |

## Upstream Pull

`upstream_pull` lets every replica fetch the same complete runtime config from
the control API without requiring the control API to reach replica-private
addresses. It is intended for a secret-bearing endpoint: the dedicated token is
sent only in an `Authorization: Bearer` header over HTTPS. Redirects are refused
so the credential cannot be forwarded to another origin.

| Field | Default | Use |
| --- | --- | --- |
| `upstream_pull.url` | required | HTTPS URL returning schema version 1 with an `upstreams` array. URLs containing credentials or a fragment are rejected. |
| `upstream_pull.token` | required | Dedicated 32–256 byte machine credential. It must differ from the gateway admin token and middleware control token; do not reuse a user/admin API key. Newlines are rejected. |
| `upstream_pull.refresh_seconds` | `300` | Successful polling cadence. Replicas apply ±10% jitter; failures retry with bounded exponential backoff. |
| `upstream_pull.request_timeout_seconds` | `90` | Whole-request timeout, including response transfer. Responses larger than 4 MiB are rejected. |

A pulled config is parsed, validated, and built completely before the active
file and in-memory router are atomically replaced. Invalid responses and HTTP or
TLS failures retain the last valid local config. An unchanged digest is not
rewritten. On a new replica whose local config is empty, failure of the initial
pull aborts startup so an empty router cannot enter the load-balancer pool.

## Middleware

The optional `middleware` section runs the middleware in the request
path. When present, the gateway consults a control plane at `control_url` to
authorize and route each request, shapes the provider request, injects response
cost, and reports usage back to the control plane — all in-process, with no
out-of-process hop. When the section is omitted the gateway serves directly.

| Field | Default | Use |
| --- | --- | --- |
| `middleware.control_url` | required | Base URL of the control plane the gateway consults for routing, authorization, catalogs, and usage reporting. |
| `middleware.control_token` | unset | Bearer token sent to the control plane. When unset, no `Authorization` header is sent. |
| `middleware.control_timeout_ms` | `60000` | Timeout for the pre-request consult and catalog fetches. A failed or timed-out consult fails closed. |
| `middleware.control_post_timeout_ms` | `10000` | Timeout for the fire-and-forget post-request usage report. |
| `middleware.sse_keepalive_ms` | `5000` | Keep-alive interval for streaming responses, measured from the start of the upstream forward. A streaming request with no upstream response headers after one interval is committed as `200 text/event-stream` and heartbeated (`: PROCESSING`) until the upstream answers; a later forward failure is delivered as the surface's in-band error event whose `code`/`type` is the status the request would otherwise have carried, while the usage report keeps the real status. A response committed this early carries no `x-receipt-id` header — when the upstream answers and the stream finalizes, the receipt is issued and fetchable by the response `id`, but an early-committed stream whose forward fails never drafts one. E2EE requests and requests carrying an ACI constraint (`provider.aci_verified` — the aci CLI's default — or pinned session ids) are never committed early, and neither is a request whose current candidate has already failed once (a same-route retry usually ends in a relayable HTTP status, 429 above all, which an early 200 would demote to an in-band error). Once a stream is open the same interval drives idle heartbeats. `0` disables the heartbeat and the pre-upstream commit with it. |
| `middleware.prefix_hash_secret` | unset | HMAC key for the consult prefix hash (the cache-affinity key). When set, it must contain at least 32 bytes of randomly generated secret material (after trimming whitespace) — anything shorter fails startup, because HMAC under a weak key is as computable as the plain hash it claims to improve on. The hash is then HMAC-SHA256(secret, prefix), so the control plane cannot dictionary-test guessed prompts — it carries no content signal beyond prefix equality. Every gateway replica must share the same value, or affinity silently fragments per replica; rotating it invalidates live affinity keys, which roll off within their 600s TTL. Unset falls back to plain SHA-256: prefix equality stays linkable and a fully-known 4KB template can be confirmed by hashing it. Either way the hash is only sent when the canonical prefix fills its 4KB cap — shorter (dictionary-enumerable) prefixes are never keyed. |
| `middleware.send_request_features` | `true` | Extract content-derived request features (a low-biased token-count estimate — deliberately under real tokenizer output on ordinary text, but a heuristic, not a guaranteed bound; the control plane may steer on it but never empties a candidate list on it — plus input modalities, tools/response-format flags, reasoning intent, and a prefix hash for cache affinity) and send them in the pre-request consult. Content never leaves the gateway — only numbers, closed enums and a one-way hash. `false` restores the featureless consult body byte-for-byte; it is the rollback lever if extraction misbehaves. |
| `middleware.tee_only_domains` | `[]` | Hosts (matched against the request `Host` header, case-insensitive) that serve TEE models only. On these hosts the model catalog is forced to `?tee=true`, a non-TEE model is refused with `404` at the pre-consult (before any forward), and serving is forced to attested (`aci_verified`) upstreams — a client cannot opt out via `provider.aci_verified:false`. Two predicates apply by design: the catalog/consult gate uses the model's `is_tee` capability flag, while serving is enforced against the deployment's attestation, so a listed `is_tee` model with no live attested deployment still fails closed (`503`). Empty (the default) leaves every host unrestricted. |

Request outcome observation is always on and needs no configuration: every
failed request that reaches the middleware completion path (consult denials,
routing/shaping failures, upstream errors, stream failures, client
disconnects; final 429s excepted, they are recorded per-attempt in the usage
pipeline) emits a `request_outcome` tracing line carrying the client-facing
and upstream status, route, attempt chain length, TTFT/duration, finish
reasons, and terminal marker. Requests rejected before that path — malformed
JSON, E2EE setup failures — do not produce lines, so
complete request accounting still needs the usage pipeline; an oversized body
does emit a `phase=body_too_large` line carrying the request id, and is
answered with the surface's JSON `413` envelope (the Anthropic envelope shape
also carries the id; the OpenAI shape, matching the upstream wire format,
does not).
A client that disconnects before the upstream's first byte is reported to the
usage pipeline as a `499` with the route that was in flight and no TTFT; a
gateway-enforced connect or read deadline is reported as a `504`, per attempt
and as the client-facing status. Consult denials
that carry a key identity (`userId` on the pre-consult response), every
429/5xx denial, and the no-route 404 are also reported to the usage
pipeline (`errorSource: "control"`, no route) so the control plane can
account for them; unauthenticated denials (401/402/403) are trace-only. A
request emits
at most one primary line; a late receipt/E2EE finalization failure appends
one supplemental `phase=finalize_error` line for the same `request_id`
(aggregate by unique request id, letting `finalize_error` supersede).
Completed
responses are logged only when their finish reasons fall outside the standard
OpenAI/Anthropic set (`anomalous_finish=true`) — the "error smuggled through a
success" class. The `detail` field (a 240-char snippet of the upstream error
body, which may quote request fragments) is emitted only when the
`request_outcome` target is enabled at `debug`; at the default level it is
blank. Silence or re-route the target via `RUST_LOG` (the subscriber uses
`EnvFilter`).

```json
{
  "middleware": {
    "control_url": "https://control.example",
    "control_token": "<control-plane-bearer-token>"
  }
}
```

Only `control_url` is required.

## Source Provenance

Source provenance is not a gateway config field. The gateway reports source
provenance from the dstack git-launcher pin at
`/etc/git-launcher/gateway.conf`:

```text
REPO_URL=https://github.com/Dstack-TEE/private-ai-gateway.git
COMMIT_SHA=<audited-full-40-or-64-hex-commit-sha>
WORK_DIR=/var/lib/git-launcher/private-ai-gateway
```

When the launcher config is absent, source provenance is unknown and the
gateway omits `source_provenance` from attestation reports. Production
deployments should use `git-launcher`. The native ACI-service verifier checks
that `attestation.evidence.app_compose` hashes to the `compose-hash` event bound
into RTMR3. Binding the reported repository commit or image digest to reviewed
source remains a verifier-policy TODO.

The canonical attestation endpoint publishes the raw measured `app_compose`.
Never place plaintext tokens, API keys, or passwords in Compose. Use Phala
encrypted environment variables and leave only variable references in the
measured file. Their encrypted values are not published or bound by
`app_compose`.

If the launcher config exists, `COMMIT_SHA` must be a full 40- or 64-character
hexadecimal commit hash. Branch names, tags, and short hashes are rejected at
startup.

## TLS Binding

TLS binding is optional. Configure it only when clients verify the gateway's
public TLS certificate SPKI from the attested keyset.

| Field | Use |
| --- | --- |
| `tls.domain_certificates` | One mounted leaf certificate per public hostname. |

For multi-domain listening, use `tls.domain_certificates`:

```json
{
  "tls": {
    "domain_certificates": [
      {
        "domain": "api.example.com",
        "certificate_path": "/run/certs/api.pem"
      },
      {
        "domain": "chat.example.com",
        "certificate_path": "/run/certs/chat.pem"
      }
    ]
  }
}
```

Raw SPKI digest inputs are not supported. The gateway reads mounted leaf
certificates, computes `sha256(SPKI)`, and publishes those digests in the
attested keyset. When `tls.domain_certificates` is configured, the request
`Host` selects the matching downstream TLS binding for
`/v1/aci/attestation`. Unknown hosts return `404 not_found`.

## Upstream Config

The upstream seed file and active upstream database use the same JSON shape: an
array of upstream entries. The seed file is deployment-owned and read-only. The
active file at `<state_dir>/upstreams.json` is gateway-owned and is replaced by
the admin API.

```json
[
  {
    "name": "route-a",
    "provider": "aci-service",
    "base_url": "https://upstream-a.example",
    "models": {
      "public-model": "provider-model"
    },
    "accepted_subjects": ["app-id:0x<measured-app-id>"],
    "accepted_dstack_kms_root_public_keys": ["<kms-root-public-key>"]
  }
]
```

Supported `provider` values:

| Provider | Use |
| --- | --- |
| `openai-compatible` | Generic OpenAI-compatible upstream with no provider-owned verifier. |
| `aci-service` | ACI service that exposes dstack/DCAP evidence. |
| `tinfoil` | Tinfoil provider adapter. |
| `near-ai` | NEAR AI provider adapter. |
| `chutes` | Chutes provider adapter. |
| `secret-ai` | Direct SecretAI SecretVM origin with optional workload pinning; see [SecretAI verification](providers/secret-ai/verification.md). |
| `phala-direct` | Direct Phala dstack-vllm-proxy endpoint. |

Provider verification policy belongs on the upstream entry. For ACI service
routes, configure accepted keyset subjects, image digests, or dstack KMS
root public keys. For `aci-service` upstreams a subject anchors only in its
measured form — `app-id:0x<hex>` of the RTMR3-verified app id. The upstream
does not need to set a keyset `subject` of its own. For `secret-ai` the same
field pins measured SecretVM workload ids on that entry.

For `secret-ai`, `base_url` must be the root HTTPS inference origin. The optional
`accepted_subjects` field pins measured SecretVM workloads in this form:

```text
secretvm:<cpu-type>:<environment>:<template>:<artifacts-version>:sha256:<compose-sha256>
```

Without this field, the verifier still reconstructs and reports the exact
production workload, but does not assert that its serving software was
operator-approved. When pins are configured, a nonmatching workload fails
verification. TDX workloads must report DCAP status `UpToDate`. An SEV-SNP
origin must meet the componentwise AMD TCB minimum embedded in the verifier.

For `aci-service`, `base_url` is the HTTPS origin used for both model traffic and
`/v1/aci/attestation`. The router fetches the report through normal TLS,
derives the attested TLS SPKI binding from that report, then pins that SPKI for
the actual upstream model request.

## Environment Variables

The gateway runtime reads only these environment variables. Provider verifier
bridges may consume provider-specific environment variables such as
`DSTACK_VERIFIER_URL` or `PRIVATE_AI_VERIFIER_DIR`.

| Variable | Use |
| --- | --- |
| `PRIVATE_AI_GATEWAY_CONFIG_PATH` | Required. Selects the static gateway config file. |
| `RUST_LOG` | Tracing filter consumed by `tracing_subscriber`. |

Deployment tooling also uses these variables:

| Variable | Use |
| --- | --- |
| `PRIVATE_AI_GATEWAY_CACHE_DIR` | `entrypoint.sh` build and toolchain cache root. Defaults to `/var/lib/private-ai-gateway/cache`. |
| `CARGO_HOME` | Optional override for Cargo cache. Defaults under `PRIVATE_AI_GATEWAY_CACHE_DIR`. |
| `RUSTUP_HOME` | Optional override for Rustup state. Defaults under `PRIVATE_AI_GATEWAY_CACHE_DIR`. |
| `CARGO_TARGET_DIR` | Optional override for Cargo build output. Defaults under `PRIVATE_AI_GATEWAY_CACHE_DIR`. |
| `PRIVATE_AI_GATEWAY_REPO_COMMIT` | Used by `deploy/compose.yaml` interpolation for the git-launcher `COMMIT_SHA` pin. |
| `PRIVATE_AI_GATEWAY_ADMIN_TOKEN` | Used by `deploy/compose.yaml` interpolation for the static config's `admin_token`. |
