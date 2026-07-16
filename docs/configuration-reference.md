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
updates `upstreams.json` through `PUT /v1/admin/upstreams`.

Unknown fields in the static gateway config are rejected at startup.

## Minimal Config

This is the smallest practical container config.

```json
{
  "bind": "0.0.0.0:8086",
  "state_dir": "/var/lib/private-ai-gateway",
  "upstream_config_seed_path": "/etc/private-ai-gateway/upstreams.seed.json",
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
| `admin_token` | unset | Bearer token for `GET` and `PUT /v1/admin/upstreams`. When unset, the admin API is not exposed. |
| `api_token` | unset | Optional Bearer token for public OpenAI-compatible catalog and inference endpoints, including `/v1/models`, `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`, `/v1/messages`, `/v1/responses`, and `/v1/metrics`. When unset, these endpoints keep the upstream default unauthenticated behavior. When set, requests without the matching Bearer token fail closed. |
| `dstack_endpoint` | dstack SDK default | dstack SDK endpoint, such as `unix:/var/run/dstack.sock`. |
| `middleware` | unset | Optional middleware section. When present, the gateway runs one configured middleware backend; when unset it serves directly. See [Middleware](#middleware). |

## Middleware

The optional `middleware` section runs a middleware backend in the request path.
The backend is selected by `middleware.mode`.

`control` mode is the existing control-plane integration. The gateway consults
a control plane at `control_url` to authorize and route each request, shapes the
provider request, injects response cost, and reports usage back to the control
plane. The gateway still performs the verified upstream forward itself.

`proxy` mode is for request-body-aware data-plane middleware, such as a vLLM
Router wrapper. The gateway stores a one-use request context, sends an
OpenAI-compatible shaped request body to `proxy_url`, and expects the middleware
to call back to `POST /internal/forward` with the chosen target route. PAG-only
routing fields such as `provider` are stripped before the proxy hop. The
callback body is not trusted as the final provider-shaped backend request; PAG
rebuilds that body from the stored original request and the selected route's
`format`/`engine`, then returns through the normal verified-forward and receipt
path. The final receipt still records `middleware.forwarded`, `route.selected`,
`request.forwarded`, and `upstream.verified`.

When the section is omitted the gateway serves directly.

Setting `api_token` also enables public model-id validation for inference
requests. In `proxy` middleware mode, model-id validation is enabled even when
`api_token` is unset so the external router receives only configured public
model ids.

| Field | Default | Use |
| --- | --- | --- |
| `middleware.mode` | `control` | `control` for `/consult/pre` candidate ordering, or `proxy` for full data-plane middleware. |
| `middleware.control_url` | required in `control` mode | Base URL of the control plane the gateway consults for routing, authorization, catalogs, and usage reporting. |
| `middleware.control_token` | unset | Bearer token sent to the control plane. When unset, no `Authorization` header is sent. |
| `middleware.control_timeout_ms` | `60000` | Timeout for the pre-request consult and catalog fetches. A failed or timed-out consult fails closed. |
| `middleware.control_post_timeout_ms` | `10000` | Timeout for the fire-and-forget post-request usage report. |
| `middleware.proxy_url` | required in `proxy` mode | Base URL of the external data-plane middleware. Public completion requests are forwarded to this URL with the original `/v1/...` path. |
| `middleware.internal_token` | required in `proxy` mode | Shared secret required on `POST /internal/forward`. Give this only to the data-plane middleware. |
| `middleware.proxy_timeout_ms` | `1800000` | Timeout for the external proxy request. |
| `middleware.sse_keepalive_ms` | `10000` | Idle keep-alive interval for streaming responses finalized by the gateway; `0` disables the heartbeat. |

```json
{
  "middleware": {
    "mode": "control",
    "control_url": "https://control.example",
    "control_token": "<control-plane-bearer-token>"
  }
}
```

Proxy mode:

```json
{
  "middleware": {
    "mode": "proxy",
    "proxy_url": "http://vllm-router:8100",
    "internal_token": "<shared-internal-callback-token>"
  }
}
```

When the proxy is a `vllm-project/router` wrapper, regular router mode still
needs at least one initial worker. Use the wrapper's bootstrap worker as the
initial `--worker-urls` value, then add and remove real upstream nodes through
the wrapper admin API so the wrapper can keep PAG upstream config and router
workers in sync.

In proxy mode the middleware receives:

```text
X-Private-AI-Gateway-Request-ID: <one-use request id>
X-Private-AI-Gateway-Internal-Token: <internal_token>
X-User-Tier: basic|premium, when known
```

The middleware callback must send:

```text
POST /internal/forward
X-Private-AI-Gateway-Internal-Token: <internal_token>
X-Private-AI-Gateway-Request-ID: <same request id>
X-Private-AI-Gateway-Targets: <selected route id>
X-Private-AI-Gateway-Target-Format: openai|anthropic
X-User-Tier: basic|premium, when known
```

The callback request body should be the request body received from the proxy
hop. PAG uses the one-use request id and selected route as the authority, then
applies its own provider request shaping before forwarding to the upstream.

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
deployments should use `git-launcher`; relying parties should compare reported
source provenance, when present, with the `REPO_URL` and `COMMIT_SHA` covered by
the attested dstack compose.

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
`/v1/attestation/report`. Unknown hosts return `404 not_found`.

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
    "accepted_workload_ids": ["<workload-id>"],
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
| `phala-direct` | Direct Phala dstack-vllm-proxy endpoint. |

Provider verification policy belongs on the upstream entry. For ACI service
routes, configure accepted workload ids, image digests, or dstack KMS root
public keys on that entry.

For `aci-service`, `base_url` is the HTTPS origin used for both model traffic and
`/v1/attestation/report`. The router fetches the report through normal TLS,
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
