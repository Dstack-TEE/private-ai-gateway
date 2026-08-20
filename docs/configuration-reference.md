# Configuration Reference

Private AI Gateway reads one static JSON file at startup and owns one writable
state directory. Upstream routes have a separate JSON schema because operators
can replace them at runtime.

Unknown fields are rejected in both schemas.

## Static Gateway Config Fields

Set `PRIVATE_AI_GATEWAY_CONFIG_PATH` to a readable JSON file. The binary does
not expose the individual static fields as environment variables. Provider
verifier child processes use the bridge variables listed under
[Environment Variables](#environment-variables).

Operators configure `state_dir`, not the individual writable files inside it.
The gateway creates the directory on startup and owns the files within it.

Minimal container configuration:

```json
{
  "bind": "0.0.0.0:8086",
  "state_dir": "/var/lib/private-ai-gateway",
  "dstack_endpoint": "unix:/var/run/dstack.sock"
}
```

### Static fields

| Field | Type | Default | Contract |
| --- | --- | --- | --- |
| `bind` | string | `127.0.0.1:8086` | TCP listener address. The gateway serves HTTP and does not terminate TLS. |
| `state_dir` | string | `/var/lib/private-ai-gateway` | Writable directory owned by one gateway process. An empty string is rejected. |
| `upstream_config_seed_path` | string | unset | Read-only upstream JSON copied to `<state_dir>/upstreams.json` only when the active file is missing or whitespace-only. |
| `upstream_pull` | object | unset | Authenticated HTTPS source for the complete runtime upstream config. See [Upstream pull](#upstream-pull). |
| `admin_token` | string | unset | Bearer token for the upstream admin API. Admin routes return `404` when unset. |
| `keyset_not_after_seconds` | positive integer | `2592000` | Lifetime of a newly resolved workload keyset. Zero is rejected. |
| `subject` | string | unset | Optional policy-interpreted workload-keyset subject. The gateway publishes it but generic verifiers do not trust it without an acceptance policy. |
| `direct_serving` | boolean | `false` | Report `service_capabilities.serving: "direct"` for a workload that performs inference itself and has no upstream hop. |
| `enable_e2ee` | boolean | `true` | Advertise and terminate the [E2EE v2 compatibility extension](../spec/e2ee-v2.md). When false, the report advertises no supported E2EE versions and v2 requests fail. |
| `tls` | object | empty | Downstream certificate bindings published in the attested keyset. See [Downstream TLS binding](#downstream-tls-binding). |
| `dstack_endpoint` | string | dstack SDK default | dstack SDK endpoint. `unix:/path` and `unix:///path` are normalized to `/path`; HTTP endpoints pass through to the SDK. |
| `middleware` | object | unset | Enables the in-process middleware and external control-plane client. See [Middleware fields](#middleware-fields). |

### Runtime state files

The gateway derives these paths from `state_dir`:

| Path | Mutability | Purpose |
| --- | --- | --- |
| `upstreams.json` | Replaced through the admin API or `upstream_pull` | Active upstream routes and credentials. |
| `sessions.jsonl` | Append and periodic compaction | Attested-session records. |
| `sessions.jsonl.lock` | Advisory lock | Prevents two gateway processes from owning one session log. |

The gateway compacts `sessions.jsonl` before serving and once per hour. It
skips malformed or tampered records during replay, removes expired records, and
rewrites live records through an atomic rename.

Do not share one state directory between running gateway processes. The second
process fails to acquire the session-log lock.

### Seed behavior

When `upstream_config_seed_path` is set, startup follows this order:

1. Read `<state_dir>/upstreams.json` if it exists.
2. Keep it when it contains any non-whitespace bytes.
3. Otherwise read and validate the seed.
4. Copy the validated seed to the active path.

An existing active file wins even if a new deployment changes the seed. Replace
the active config through `PUT /v1/admin/upstreams` or reset the state volume as
an explicit operator action.

## Upstream pull

`upstream_pull` lets each replica fetch the complete runtime config without
requiring the control API to reach replica-private addresses. The gateway sends
the dedicated token only in an `Authorization: Bearer` header over HTTPS and
refuses redirects so the credential cannot move to another origin.

| Field | Type | Default | Contract |
| --- | --- | --- | --- |
| `upstream_pull.url` | string | required | HTTPS URL returning schema version 1 with an `upstreams` array. Credentials and fragments are rejected. |
| `upstream_pull.token` | string | required | Dedicated 32 to 256 byte machine credential. It must differ from `admin_token` and `middleware.control_token`. Newlines are rejected. |
| `upstream_pull.refresh_seconds` | positive integer | `300` | Successful polling cadence. Replicas apply ±10% jitter; failures retry with bounded exponential backoff. |
| `upstream_pull.request_timeout_seconds` | positive integer | `90` | Whole-request timeout, including response transfer. Responses larger than 4 MiB are rejected. |

A pulled config is parsed, validated, and built completely before the active
file and in-memory router are atomically replaced. Invalid responses and HTTP
or TLS failures retain the last valid local config. An unchanged digest is not
rewritten. On a new replica whose local config is empty, failure of the initial
pull aborts startup so an empty router cannot enter the load-balancer pool.

## Downstream TLS binding

The gateway does not serve TLS, but it can attest the leaf certificate keys
used by a TLS terminator in the same reviewed deployment. Configure one mounted
leaf certificate for each public hostname:

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

| Field | Contract |
| --- | --- |
| `tls.domain_certificates` | Array of unique domain and certificate entries. An empty array disables configured downstream bindings. |
| `tls.domain_certificates[].domain` | Hostname without a scheme, port, path, whitespace, comma, or trailing dot. Matching is lowercase. |
| `tls.domain_certificates[].certificate_path` | Non-empty path to a PEM or DER leaf certificate readable at startup. |

At startup, the gateway parses each first PEM certificate (or the DER file),
computes `SHA256(SubjectPublicKeyInfo)`, and places the digest and domain in the
workload keyset. Raw digest input is not supported.

When at least one domain binding exists, both canonical and legacy attestation
handlers require a `Host` that matches a configured domain. The report includes
the selected `attestation.evidence.downstream_tls_binding`. Unknown or malformed
hosts return `404` instead of an unbound report.

TLS issuance, renewal, SNI routing, and private-key custody remain deployment
responsibilities. A verifier must confirm that the certificate served to the
client matches the SPKI selected in the report.

## Middleware fields

The optional middleware runs in the gateway process. It calls an external
control plane over HTTP or HTTPS and then calls the ACI service in-process.

```json
{
  "middleware": {
    "control_url": "https://control.example",
    "control_token": "<control-plane-bearer-token>",
    "tee_only_domains": ["confidential.example.com"]
  }
}
```

| Field | Type | Default | Contract |
| --- | --- | --- | --- |
| `middleware.control_url` | string | required | Non-empty base URL. `/consult/pre`, `/consult/post`, and catalog paths are appended to it. |
| `middleware.control_token` | string | unset | Optional bearer token sent to the control plane. Blank strings are treated as unset. |
| `middleware.control_timeout_ms` | integer | `60000` | Timeout for pre-consult and catalog requests. A failed pre-consult denies the inference request. |
| `middleware.control_post_timeout_ms` | integer | `10000` | Timeout for post-request usage reports. Failure does not change a served response. |
| `middleware.sse_keepalive_ms` | integer | `10000` | Idle SSE comment interval. Zero disables keep-alive comments. |
| `middleware.prefix_hash_secret` | string | unset | HMAC key for the consult prefix hash. After trimming, it must contain at least 32 bytes. Every replica must share the same value. When unset, the gateway uses plain SHA-256, which leaves prefix equality linkable. |
| `middleware.send_request_features` | boolean | `true` | Send content-derived features in pre-consult: a low-biased token estimate, closed-enum modalities, tool and response-format flags, reasoning intent, and an optional prefix hash. No prompt text is sent. Set false to restore the featureless consult body. |
| `middleware.tee_only_domains` | string array | `[]` | Hostnames whose catalog queries force `tee=true` and whose inference requests require an ACI-verified route. Matching uses the normalized HTTP `Host`. |

The control plane must implement the
[control-plane contract](control-plane-contract.md). In particular, it must
deny non-TEE models when a pre-consult contains `tee: true` if the deployment
expects a `404` at the catalog and authorization layer. The gateway still
enforces successful upstream verification before serving any request on a
TEE-only hostname.

### Request outcome logs

Middleware mode emits structured `request_outcome` tracing records for terminal
failures and anomalous finish reasons. The default info-level record contains
statuses, route, phase, timing, and sanitized identifiers. Raw upstream detail
is blank unless `RUST_LOG` enables `request_outcome=debug`; that detail can
contain provider error text and fragments of client input.

Malformed JSON, body-limit rejections, and E2EE setup failures occur before the
middleware completion path and do not emit a `request_outcome` record. Use the
control-plane usage pipeline when complete accounting is required. Consult
denials carrying `userId`, every `429` or `5xx` consult denial, and the no-route
`404` are also reported to that pipeline with `errorSource: "control"` and no
route. Unauthenticated `401`, `402`, and `403` denials remain trace-only.

A request emits at most one primary outcome. A late receipt or E2EE
finalization error adds one `phase=finalize_error` record with the same
`request_id`; aggregators should let that record supersede the primary outcome.

## Upstream configuration

The seed and active upstream files use a JSON array. Each entry owns one
provider origin and maps one or more public model IDs to provider model IDs.

```json
[
  {
    "name": "tinfoil-primary",
    "provider": "tinfoil",
    "base_url": "https://inference.tinfoil.sh",
    "models": {
      "confidential-chat": "kimi-k2-6"
    },
    "bearer_token": "<provider-api-key>"
  }
]
```

In direct mode, the public model ID selects the first configured route for that
model. Middleware route IDs have this exact form:

```text
<upstream name>:<public model ID>
```

The gateway rewrites the request's top-level `model` to the provider model ID
before provider verification and forwarding. The receipt commits to both the
client-observed body and the provider-facing body.

### Provider values

| Value | Classification | Transport and verifier |
| --- | --- | --- |
| `openai-compatible` | non-TEE | OpenAI-compatible HTTP with no provider verifier. |
| `anthropic` | non-TEE | Native Anthropic HTTP using `x-api-key` and `anthropic-version: 2023-06-01`; requires `path`. |
| `aci-service` | TEE | Native Rust ACI report, dstack/DCAP, KMS-custody, and TLS-SPKI verifier. |
| `tinfoil` | TEE | Tinfoil verifier through the Python bridge and TLS-SPKI enforcement. |
| `near-ai` | TEE | NEAR AI verifier through the Python bridge, external dstack verifier, and TLS-SPKI enforcement. |
| `chutes` | TEE | Per-instance attestation and encrypted Chutes E2EE transport. |
| `secret-ai` | TEE | SecretVM CPU, GPU, workload, and inference-SPKI verifier. |
| `phala-direct` | TEE | Direct dstack-vllm-proxy verification through the Python bridge and TLS-SPKI enforcement. |

TEE classification makes a route eligible for `provider.aci_verified`. A
successful provider verifier and enforceable binding are still required at
request time.

### Upstream fields

| Field | Type | Default | Contract |
| --- | --- | --- | --- |
| `name` | string | required | Unique non-empty upstream name. |
| `provider` | enum | `openai-compatible` | One provider value from the table above. |
| `base_url` | string | required | Non-empty provider origin. `secret-ai` requires a root HTTPS URL without user info, path, query, or fragment. |
| `path` | string | unset | Upstream path for chat-shaped surfaces. Leading `/` is added when missing. `anthropic` requires a non-empty path, normally `/v1/messages`. Other surfaces retain their public path. |
| `models` | object | required | Non-empty map of public model ID to non-empty provider model ID. |
| `bearer_token` | string | unset | Provider credential. The gateway never returns its value from the admin API. For `anthropic`, this becomes `x-api-key`. |
| `basic_auth` | boolean | `false` | Send `Authorization: Basic <bearer_token>`. Allowed only for `openai-compatible` and `chutes`, and requires a token. |
| `accepted_subjects` | string array | unset | Accepted measured ACI-service subjects, or optional SecretAI measured-workload pins. For ACI service, use `app-id:0x<hex>` values derived from RTMR3-verified evidence. |
| `accepted_image_digests` | string array | unset | ACI-service source image allowlist. |
| `accepted_dstack_kms_root_public_keys` | string array | unset | ACI-service accepted dstack KMS root public keys. |
| `pccs_url` | string | Phala PCCS from `dcap_qvl` | PCCS used by the native ACI-service DCAP verifier. |
| `verifier_cache_seconds` | positive integer | `300` | Provider-verification cache lifetime. Zero is rejected. |
| `connect_timeout_seconds` | positive integer | `10` | Upstream HTTP connect timeout. Zero is rejected. |
| `read_timeout_seconds` | positive integer | `600` | Upstream HTTP read timeout. Zero is rejected. |
| `verifier_request_timeout_seconds` | positive integer | `60` | Provider verification timeout. Zero is rejected. |
| `verification_refresh_seconds` | integer | `max(verifier_cache_seconds - 60, 1)` | Background verification refresh cadence. Zero disables proactive refresh for this entry. |
| `session_refresh_seconds` | integer | `45` for Chutes; disabled otherwise | Chutes nonce-session refresh cadence. Zero disables it. |
| `chutes_e2ee_api_base` | string | `https://api.chutes.ai` | Chutes discovery, evidence, and E2EE API base. Chutes only. |
| `chutes_chute_ids` | object | unset | Map of provider model ID to chute UUID. Keys must appear in `models` values. Chutes only. |
| `chutes_e2ee_discovery_rounds` | integer from 1 to 10 | `3` | Evidence discovery attempts per verification. Chutes only. |
| `chutes_e2ee_discovery_interval_seconds` | non-negative integer | `0` | Delay between discovery rounds. Chutes only. |

An `aci-service` entry must provide at least one accepted subject or image
digest and at least one accepted KMS root public key. The verifier rejects an
empty acceptance policy. The upstream keyset does not need to self-assert the
accepted subject; the verifier derives the `app-id:0x<hex>` subject from
measured evidence.

Chutes-specific fields on another provider are rejected. For private Chutes
origins that use Basic authentication, follow the
[private Chutes configuration](providers/chutes/configuration.md).

### Empty configuration

A missing, empty, or whitespace-only `upstreams.json` parses as an empty route
list. An explicit empty array has the same meaning. The identity, attestation,
metrics, and admin endpoints remain available; inference model routing fails
until routes are configured.

## Admin API

When `admin_token` is set, inspect the active config:

```bash
curl --fail --silent --show-error \
  -H "Authorization: Bearer $PRIVATE_AI_GATEWAY_ADMIN_TOKEN" \
  http://127.0.0.1:8086/v1/admin/upstreams
```

The response includes `config_path`, a JCS SHA-256 `config_digest`, and redacted
entries. It replaces `bearer_token` with `bearer_token_configured: true|false`.

Replace the config atomically:

```bash
curl --fail --silent --show-error \
  -X PUT \
  -H "Authorization: Bearer $PRIVATE_AI_GATEWAY_ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  --data-binary @upstreams.json \
  http://127.0.0.1:8086/v1/admin/upstreams
```

The gateway validates the complete array before writing a temporary file and
renaming it over the active path. It swaps the in-memory router and verifier
state after the write succeeds, then starts verification prewarm in the
background.

## Source provenance

Source provenance is not a JSON config field. The binary reads
`/etc/git-launcher/gateway.conf` when present and accepts:

```text
REPO_URL=https://github.com/Dstack-TEE/private-ai-gateway.git
COMMIT_SHA=<full-40-or-64-character-hex-commit>
WORK_DIR=/var/lib/git-launcher/private-ai-gateway
```

`REPO_URL` and `COMMIT_SHA` are required when the file exists. A branch, tag,
short hash, or non-hex commit is rejected. When the launcher file is absent, the
report omits source provenance.

The report publishes the measured `app_compose` preimage. The native ACI-service
verifier checks that it hashes to the RTMR3-bound `compose-hash` event. That
integrity check does not decide whether the launcher, repository revision,
image, compiler, or dependencies are approved. Verifier policy must make those
acceptance decisions.

Never put plaintext credentials in measured Compose content. Use deployment
secret facilities and keep only variable references in the manifest.

## Environment Variables

The gateway process and provider-verifier children use:

| Variable | Use |
| --- | --- |
| `PRIVATE_AI_GATEWAY_CONFIG_PATH` | Required path to the static gateway config. |
| `RUST_LOG` | `tracing_subscriber` filter. Defaults to `info`. |
| `PRIVATE_AI_VERIFIER_DIR` | Optional Python verifier checkout override consumed by provider-verifier child processes. |
| `DSTACK_VERIFIER_URL` | External verifier URL consumed by NEAR AI and PhalaDirect bridge code. Those adapters default to `http://localhost:8080` when unset. |

The repository entrypoint and deployment manifest also use:

| Variable | Use |
| --- | --- |
| `PRIVATE_AI_GATEWAY_CACHE_DIR` | Toolchain and build cache root. Defaults to `/var/lib/private-ai-gateway/cache`. |
| `CARGO_HOME` | Cargo cache override used by `entrypoint.sh`. |
| `RUSTUP_HOME` | Rustup state override used by `entrypoint.sh`. |
| `CARGO_TARGET_DIR` | Cargo output override used by `entrypoint.sh`. |
| `PRIVATE_AI_GATEWAY_REPO_COMMIT` | Compose interpolation for the git-launcher source pin. |
| `PRIVATE_AI_GATEWAY_ADMIN_TOKEN` | Compose interpolation for the static config's admin token. |

Provider credentials belong in the upstream config or the deployment mechanism
that renders it. The live test harness reads its own provider key variables; see
the [testing guide](live-e2e-test-suite.md).
