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
| `dstack_endpoint` | dstack SDK default | dstack SDK endpoint, such as `unix:/var/run/dstack.sock`. |
| `middleware` | unset | Optional middleware section. When present, the gateway consults a control plane to route and authorize each request and applies request/response transforms; when unset it serves directly. See [Middleware](#middleware). |

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
| `middleware.sse_keepalive_ms` | `10000` | Idle keep-alive interval for streaming responses; `0` disables the heartbeat. |

Request outcome observation is always on and needs no configuration: every
failed request that reaches the middleware completion path (consult denials,
routing/shaping failures, upstream errors, stream failures, client
disconnects; final 429s excepted, they are recorded per-attempt in the usage
pipeline) emits a `request_outcome` tracing line carrying the client-facing
and upstream status, route, attempt chain length, TTFT/duration, finish
reasons, and terminal marker. Requests rejected before that path — malformed
JSON, oversized bodies, E2EE setup failures — do not produce lines, so
complete request accounting still needs the usage pipeline. A request emits
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
| `secret-ai` | Direct SecretAI SecretVM origin with optional workload pinning; see [SecretAI verification](providers/secret-ai/verification.md). |
| `phala-direct` | Direct Phala dstack-vllm-proxy endpoint. |

Provider verification policy belongs on the upstream entry. For ACI service
routes, configure accepted workload ids, image digests, or dstack KMS root
public keys on that entry.

For `secret-ai`, `base_url` must be the root HTTPS inference origin. The optional
`accepted_workload_ids` field pins measured SecretVM workloads in this form:

```text
secretvm:<cpu-type>:<environment>:<template>:<artifacts-version>:sha256:<compose-sha256>
```

Without this field, the verifier still reconstructs and reports the exact
production workload, but does not assert that its serving software was
operator-approved. When pins are configured, a nonmatching workload fails
verification. TDX workloads must report DCAP status `UpToDate`. An SEV-SNP
origin must also configure a reviewed componentwise minimum:

```json
"minimum_sev_tcb": {
  "boot_loader": 10,
  "tee": 0,
  "snp": 23,
  "microcode": 88
}
```

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
