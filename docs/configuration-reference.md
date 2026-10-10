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
| `admin_token_sha256` | hex string | unset | SHA-256 the admin token must match, whether it comes from `admin_token`, `PRIVATE_AI_GATEWAY_ADMIN_TOKEN`, or `PRIVATE_AI_GATEWAY_ADMIN_TOKEN_FILE`. Startup fails on a missing or mismatched token. |
| `keyset_not_after_seconds` | positive integer | `2592000` | Lifetime of a newly resolved workload keyset. Zero is rejected. |
| `subject` | string | unset | Optional policy-interpreted workload-keyset subject. The gateway publishes it but generic verifiers do not trust it without an acceptance policy. |
| `direct_serving` | boolean | `false` | Report `service_capabilities.serving: "direct"` for a workload that performs inference itself and has no upstream hop. Setting it together with `middleware` is a startup error. |
| `enable_e2ee` | boolean | `true` | Advertise and terminate the [E2EE v2 compatibility extension](../spec/e2ee-v2.md). When false, the report carries `supported_e2ee_versions: []` and v2 requests fail with `400 e2ee_invalid_version`. The legacy `X-Signing-Algo` E2EE path is not affected. |
| `tls` | object | empty | Downstream certificate bindings published in the attested keyset. See [Downstream TLS binding](#downstream-tls-binding). |
| `dstack_endpoint` | string | dstack SDK default | dstack SDK endpoint. `unix:/path` and `unix:///path` are normalized to `/path`; HTTP endpoints pass through to the SDK. |
| `middleware` | object | unset | Enables the in-process middleware and external control-plane client. See [Middleware fields](#middleware-fields). |
| `privatemode_proxy` | object | unset | Static policy for an official Privatemode proxy co-deployed in the measured Compose. Required before a `privatemode` route can load. See [Privatemode proxy](#privatemode-proxy). |
| `upstream_verification_concurrency` | positive integer | `4` | Maximum concurrent background verifier-cache refreshes, including initial verification. Limits pressure on the shared verifier sidecar; request-time verification is independent. |

Do not set `upstream_verification_concurrency` in production until the rollback
window has passed: the older binary rejects unknown static configuration fields.

### Runtime state files

The gateway derives these paths from `state_dir`:

| Path | Mutability | Purpose |
| --- | --- | --- |
| `upstreams.json` | Replaced through the admin API or `upstream_pull` | Active upstream routes and credentials. |
| `sessions.jsonl` | Append and periodic compaction | Attested-session records. |
| `sessions.jsonl.lock` | Advisory lock | Prevents two gateway processes from owning one session log. |

[Storage: compacted JSONL](attested-session-system.md#storage-compacted-jsonl)
describes how the gateway replays and compacts `sessions.jsonl`.

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
used by a TLS terminator inside the same attested workload. Configure one mounted
leaf certificate for each public hostname.
[Bind public TLS identities](../deploy/README.md#bind-public-tls-identities)
has a two-domain example.

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

The TLS terminator that holds these certificates' private keys runs inside the
attested workload; [Bind public TLS identities](../deploy/README.md#bind-public-tls-identities)
covers its setup and key custody.

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
| `middleware.sse_keepalive_ms` | integer | `5000` | Interval for pre-header processing comments and post-header idle heartbeats. Zero disables both. See [Streaming keepalives and early commit](#streaming-keepalives-and-early-commit). |
| `middleware.send_request_features` | boolean | `true` | Send content-derived features in pre-consult: a low-biased token estimate, closed-enum modalities, tool and response-format flags, reasoning intent, and an optional prefix hash keyed with a gateway key derived from dstack KMS. No prompt text is sent. Set false to omit the `request` block. |
| `middleware.tee_only_domains` | string array | `[]` | Hostnames that serve TEE models only, matched against the normalized HTTP `Host`. See [TEE-only hostnames](#tee-only-hostnames). |

The control plane must implement the
[control-plane contract](control-plane-contract.md).

### TEE-only hostnames

On a hostname listed in `middleware.tee_only_domains`:

- the catalog relay replaces any client `tee` query parameter with `tee=true`;
- the pre-consult carries `tee: true`, and the control plane should deny a
  non-TEE model, normally with `404`;
- inference requires ACI verification as if the client sent
  `provider.aci_verified: true`, and a client `aci_verified: false` is ignored;
- a listed model with no attested deployment fails closed with
  `503 upstream_verification_failed`.

When the list is non-empty, an inference request whose `Host` is missing or
malformed is treated as TEE-only. The component in front of the gateway must
forward the original `Host`.

### Streaming keepalives and early commit

The interval starts when the gateway begins forwarding to an upstream. If an
eligible stream has no upstream response headers after one interval, the
gateway commits `200 text/event-stream` and emits `: PROCESSING` comments until
the upstream answers. After the response opens, the same interval drives idle
heartbeats.

Only unconstrained, non-E2EE requests are eligible. The gateway does not commit
early when the request carries `provider.aci_verified`, pinned session IDs, or
E2EE headers. It also waits while the candidate in flight has already failed
once in this request, as in the delayed capacity retry, because that attempt
usually ends in an HTTP status such as `429` that the client should receive.

An early-committed response cannot carry `X-Receipt-Id` because the upstream
has not yet been selected. After a successful stream, the receipt is available
by response `id`. If forwarding fails after the early commit, the gateway sends
the surface's in-band error event, carrying the status the response would
otherwise have used, and drafts no receipt. The control-plane usage report also
records that real failure status rather than the HTTP `200` already sent to the
client.

### Failure accounting

Middleware mode accounts for failed requests in the control-plane usage
pipeline, with one report per attempt. See
[`POST /consult/post`](control-plane-contract.md#post-consultpost) for the
fields, the reported denials, and the `errorMessage` values.

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
| `privatemode` | TEE | Official Privatemode proxy co-deployed in the measured Compose; the proxy owns attestation and E2EE. Forbids `bearer_token` and `path`, and `base_url` must equal static `privatemode_proxy.base_url`. |

TEE classification makes a route eligible for `provider.aci_verified`. A
successful provider verifier and enforceable binding are still required at
request time.

### Upstream fields

| Field | Type | Default | Contract |
| --- | --- | --- | --- |
| `name` | string | required | Unique non-empty upstream name. |
| `provider` | enum | `openai-compatible` | One provider value from the table above. |
| `base_url` | string | required | Non-empty provider origin. `secret-ai` requires a root HTTPS URL without user info, path, query, or fragment. |
| `path` | string | unset | Upstream path for chat requests. Leading `/` is added when missing. See [Chat request path](#chat-request-path). |
| `models` | object | required | Non-empty map of public model ID to non-empty provider model ID. |
| `bearer_token` | string | unset | Provider credential. The gateway never returns its value from the admin API. For `anthropic`, this becomes `x-api-key`. |
| `streaming_usage` | `"final"` or `"continuous"` | unset | Deployment-wide OpenAI streaming usage flags. `final` forces `include_usage`; `continuous` also forces `continuous_usage_stats`. Omission preserves existing behavior. Applies only to streamed chat/legacy completions before receipt hashing and upstream encryption. |
| `basic_auth` | boolean | `false` | Send `Authorization: Basic <bearer_token>`. Allowed only for `openai-compatible` and `chutes`, and requires a token. |
| `accepted_subjects` | string array | unset | Accepted measured ACI-service subjects, or optional SecretAI measured-workload pins. For ACI service, use `app-id:0x<hex>` values derived from RTMR3-verified evidence. |
| `accepted_image_digests` | string array | unset | ACI-service source image allowlist. |
| `accepted_dstack_kms_root_public_keys` | string array | unset | ACI-service accepted dstack KMS root public keys. |
| `pccs_url` | string | Phala PCCS from `dcap_qvl` | PCCS used by the native ACI-service DCAP verifier. |
| `verifier_cache_seconds` | positive integer | `300` | Provider-verification cache lifetime. Zero is rejected. |
| `connect_timeout_seconds` | positive integer | `10` | Upstream HTTP connect timeout. Zero is rejected. |
| `read_timeout_seconds` | positive integer | `600` | Upstream HTTP read timeout. Zero is rejected. |
| `verifier_request_timeout_seconds` | positive integer | `60` | Provider verification timeout. Zero is rejected. |
| `verification_refresh_seconds` | integer | `max(verifier_cache_seconds - verifier_request_timeout_seconds, 1)` | Background refresh period p, measured from verification start. Default max(verifier_cache_seconds - verifier_request_timeout_seconds, 1) (240 with defaults). Zero disables background verification for this entry. |
| `session_refresh_seconds` | integer | `45` for Chutes; disabled otherwise | Chutes nonce-session refresh cadence. Zero disables it. |
| `chutes_e2ee_api_base` | string | `https://api.chutes.ai` | Chutes discovery, evidence, and E2EE API base. Chutes only. |
| `chutes_chute_ids` | object | unset | Map of provider model ID to chute UUID. Keys must appear in `models` values. Chutes only. |
| `chutes_e2ee_discovery_rounds` | integer from 1 to 10 | `3` | Evidence discovery attempts per verification. Chutes only. |
| `chutes_e2ee_discovery_interval_seconds` | non-negative integer | `0` | Delay between discovery rounds. Chutes only. |

See [Cache and background refresh](upstream-verification-lifecycle.md#cache-and-background-refresh)
for scheduling, failure retries, concurrency, and cache-warmth limits.

An `aci-service` entry must provide at least one accepted subject or image
digest and at least one accepted KMS root public key. The verifier rejects an
empty acceptance policy. The upstream keyset does not need to self-assert the
accepted subject; the verifier derives the `app-id:0x<hex>` subject from
measured evidence.

### Chat request path

Chat requests, including Anthropic `/v1/messages` requests converted to the
upstream's chat format, go to the upstream's `path`. Without `path` they go to
`/v1/chat/completions`. A native `anthropic` upstream requires `path`, normally
`/v1/messages`. `/v1/completions`, `/v1/embeddings`, and `/v1/responses` keep
their public path.

Chutes-specific fields on another provider are rejected. For private Chutes
origins that use Basic authentication, follow the
[private Chutes configuration](providers/chutes/configuration.md).

### Empty configuration

A missing, empty, or whitespace-only `upstreams.json` parses as an empty route
list. An explicit empty array has the same meaning. The identity, attestation,
metrics, and admin endpoints remain available; inference model routing fails
until routes are configured.

## Admin API

The admin API reads and replaces the active upstream config:

| Method and path | Behavior |
| --- | --- |
| `GET /v1/admin/upstreams` | Returns `config_path`, `config_digest`, and the redacted `upstreams` array. |
| `PUT /v1/admin/upstreams` | Takes a complete upstream JSON array and returns the same shape for the new active config. |

Every admin route requires `Authorization: Bearer <admin_token>`. When
`admin_token` is unset, the routes return `404`. A missing token returns `401`
and a wrong token returns `403`.

Redaction replaces each `bearer_token` with `bearer_token_configured: true` or
`false`. `config_digest` is `sha256:<hex>` over the gateway's own JSON
serialization of the active config. It is a local version stamp, not a
canonical JSON (JCS) digest.

A `PUT` validates the complete array before it writes a temporary file and
renames it over the active path. An invalid array returns `400` and leaves the
active config unchanged. After the write succeeds, the gateway swaps the
in-memory router and verifier state, then wakes the background verification
supervisor. [Configure upstreams after startup](../deploy/README.md#configure-upstreams-after-startup)
shows both calls with `curl`.

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

The report's `source_provenance` field is the gateway's own statement of these
values, not evidence. The evidence is the measured `app_compose` preimage that
the report also publishes. The native ACI-service verifier checks that it
hashes to the RTMR3-bound `compose-hash` event. That integrity check does not
decide whether the launcher, repository revision, image, compiler, or
dependencies are approved. Verifier policy must make those acceptance
decisions.

## Privatemode proxy

`privatemode_proxy` is static deployment policy, so the admin API cannot point
routes at another proxy or change the measured pins.

| Field | Contract |
| --- | --- |
| `base_url` | Internal HTTP(S) origin of the co-deployed proxy. Paths, credentials, queries, and fragments are rejected. |
| `manifest_log_path` | Absolute path to the proxy's manifest-history log, mounted read-only. The latest complete entry is reported as unbound observation metadata. |
| `credential_path` | Absolute path to the Compose secret mounted into both gateway and proxy. The gateway checks it at startup and never forwards it. |
| `credential_sha256` | SHA-256 of that credential. The renderer derives it from `PRIVATEMODE_API_KEY`; a mismatch fails startup. |
| `proxy_image_digest` | OCI digest of the proxy image pinned in the same Compose, as `sha256:<64-hex>`. |

For `privatemode`, dstack Compose owns the proxy process, image pin, shared
manifest-history volume, restart policy, and private network. Mutable routes
must match the static origin and cannot set `bearer_token` or `path`; all share
the measured Compose credential. The gateway validates the credential digest, requires the proxy's
`/readyz` readiness probe to succeed, reports the latest fetched manifest as explicitly
unbound metadata, and forwards only to the encrypted v1.48 handlers. Separate
credentials require separate measured deployments.
See [Privatemode verification](providers/privatemode/verification.md).

## Secrets and measured compose

dstack publishes the measured compose as `app_compose` in every attestation
report, so never put plaintext credentials in it. Supply secrets through the
deployment's encrypted environment, KMS, or a mounted secret, and keep only
variable references in the manifest. An encrypted variable's value is neither
published in `app_compose` nor bound by the compose hash.

## Environment Variables

The gateway process and provider-verifier children use:

| Variable | Use |
| --- | --- |
| `PRIVATE_AI_GATEWAY_CONFIG_PATH` | Required path to the static gateway config. |
| `PRIVATE_AI_GATEWAY_ADMIN_TOKEN` | Optional runtime admin token overriding static `admin_token`. |
| `PRIVATE_AI_GATEWAY_ADMIN_TOKEN_FILE` | Optional path to a file holding the admin token, such as a Compose secret, read when `PRIVATE_AI_GATEWAY_ADMIN_TOKEN` is unset. Surrounding whitespace is trimmed; an empty or unreadable file fails startup. |
| `RUST_LOG` | `tracing_subscriber` filter. Defaults to `info`. |
| `PRIVATE_AI_VERIFIER_DIR` | Optional Python verifier checkout override consumed by provider-verifier child processes. See [Install dependencies](getting-started.md#install-dependencies). |
| `DSTACK_VERIFIER_URL` | External verifier URL consumed by NEAR AI and PhalaDirect bridge code. Those adapters default to `http://localhost:8080` when unset. |

The repository entrypoint and deployment manifest also use:

| Variable | Use |
| --- | --- |
| `PRIVATE_AI_GATEWAY_CACHE_DIR` | Toolchain and build cache root. Defaults to `/var/lib/private-ai-gateway/cache`. |
| `CARGO_HOME` | Cargo cache override used by `entrypoint.sh`. Defaults to `$PRIVATE_AI_GATEWAY_CACHE_DIR/cargo`. |
| `RUSTUP_HOME` | Rustup state override used by `entrypoint.sh`. Defaults to `$PRIVATE_AI_GATEWAY_CACHE_DIR/rustup`. |
| `CARGO_TARGET_DIR` | Cargo output override used by `entrypoint.sh`. Defaults to `$PRIVATE_AI_GATEWAY_CACHE_DIR/target`. |
| `PRIVATE_AI_GATEWAY_ADMIN_TOKEN` | Compose interpolation for the static config's admin token in `compose.yaml`. `compose.privatemode.yaml` instead mounts it as a Compose secret read through `PRIVATE_AI_GATEWAY_ADMIN_TOKEN_FILE`. |
| `PRIVATE_AI_GATEWAY_ADMIN_TOKEN_SHA256` | Non-secret digest rendered into `compose.privatemode.yaml` as `admin_token_sha256`. |
| `PRIVATEMODE_API_KEY` | Encrypted deployment input mounted as one Compose secret into the gateway and proxy. The renderer derives `PRIVATEMODE_CREDENTIAL_SHA256` from it. |

Provider credentials belong in the upstream config or the deployment mechanism
that renders it. The live test harness reads its own provider key variables; see
the [testing guide](live-e2e-test-suite.md).

### Streaming usage policy

Set `streaming_usage` on an upstream entry through the existing admin upstream
configuration API. It applies to every model mapped by that entry, independently
of its provider name. Enable `continuous` only after checking that the endpoint
accepts the flag; acceptance does not guarantee partial usage frames.

The policy preserves unrelated client stream options and forces the requested
booleans true. `final` does not remove a continuous flag supplied by the client.
Neither mode adds flags to nonstream requests, Responses, embeddings, or native
Anthropic messages. When `path` is configured alongside the policy, it must end
in `/chat/completions` or `/completions`. The usage the upstream sends reaches
the client whether or not the client asked for it. With direct forwarding,
`final` adds a closing chunk with an empty `choices` array, and `continuous`
adds `usage` to every chunk.

Existing configurations without this field keep their behavior: middleware
continues requesting `include_usage`, while direct forwarding adds no new flags.
Removing the field restores that behavior.

Upgrade every gateway before sending this field. An older gateway rejects a
configuration that contains it and keeps its previous configuration. The gateway
also saves the configuration it receives, and an older gateway cannot start from
a saved configuration that contains the field. Before rolling a gateway back,
remove the field and wait until every gateway has picked up the change.
