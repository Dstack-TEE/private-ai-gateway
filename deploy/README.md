# Deploy with git-launcher

This directory contains the reference dstack deployment for Private AI Gateway. It uses `git-launcher` to check out an exact gateway commit, then runs the repository-owned `entrypoint.sh` inside the confidential VM.

The checked-in manifest is an auditable starting point. It is not a complete production platform. Authentication, rate limiting, external TLS termination, secret delivery, monitoring, backup, and availability remain deployment responsibilities.

## Deployment model

The default [`compose.yaml`](compose.yaml) runs one gateway process in direct-upstream mode:

```text
client -> external TLS terminator -> gateway :8086 -> configured providers
```

The gateway serves HTTP on port `8086`; it does not terminate TLS. Configure the optional in-process middleware and its external HTTP control plane in the static config when policy-based routing is required.

The compose pins this launcher image by digest:

```text
docker.io/dstacktee/git-launcher@sha256:4437dce18ec713b0991d34bd926d324966b1a0b90fad485b8ddb3f4ed2af138b
```

Review the launcher image, gateway commit, complete compose content, and runtime policy together. The launcher image alone does not identify the deployed workload.

## Prepare the deployment

Choose a reviewed, full 40-character gateway commit and generate a strong admin token. The admin token authorizes runtime upstream replacement.

The checked-in upstream seed is empty. Before deployment, either:

- replace the `gateway-upstreams` content in `compose.yaml` with reviewed routes; or
- keep it empty and use `PUT /v1/admin/upstreams` after the process starts.

[`upstreams.example.json`](upstreams.example.json) demonstrates current Anthropic, Tinfoil, NEAR AI, Chutes, and Phala-direct entries. It contains placeholders, not production policy.

Do not place plaintext credentials in measured compose content. Supply secrets through the deployment's encrypted environment, KMS, or mounted secret mechanism. Keep only secret-variable references in the manifest.

## One-Command Deploy

From this directory:

```sh
phala deploy -n private-ai-gateway -c compose.yaml \
  -e PRIVATE_AI_GATEWAY_REPO_COMMIT=<full-40-hex-commit> \
  -e PRIVATE_AI_GATEWAY_ADMIN_TOKEN=<long-random-admin-token>
```

For a development deployment, [`gateway.env.example`](gateway.env.example) shows the required variables. Passing individual encrypted variables is preferable for a production deployment because it avoids a plaintext secrets file.

Wait for the process to build and start, then check liveness:

```sh
curl --fail http://<gateway-host>:8086/health
```

Liveness only proves that the process is serving requests. It does not prove provider availability or successful attestation.

## Ownership boundary

The launcher is build-system agnostic. It performs four operations: clone the repository, check out `COMMIT_SHA`, preserve the container environment, and run `bash entrypoint.sh` from the pinned checkout. The compose does not set `REPO_SUBDIR` because the gateway entrypoint is at the repository root.

The gateway repository owns everything after that boundary:

| Concern | Owner | Source |
| --- | --- | --- |
| Launcher image and source pin | Deployment | `compose.yaml` and `gateway-pin` |
| Static gateway policy | Deployment | `gateway-config` in `compose.yaml` |
| Initial upstream policy | Deployment | `gateway-upstreams` in `compose.yaml` |
| Toolchain bootstrap, build, and exec | Gateway repository | `entrypoint.sh` |
| HTTP, ACI, routing, and provider verification | Gateway binary | `src/` |
| Optional routing and authorization decisions | External control plane | `middleware.control_url` |

The static gateway config is mounted at:

```text
/etc/private-ai-gateway/gateway.config.json
```

The compose selects it with:

```text
PRIVATE_AI_GATEWAY_CONFIG_PATH=/etc/private-ai-gateway/gateway.config.json
```

See [Configuration reference](../docs/configuration-reference.md) for every field and validation rule.

## Persistent volumes

| Volume | Mount | Contents |
| --- | --- | --- |
| `gateway-checkout` | `/var/lib/git-launcher` | Launcher-owned source checkout. It is scrubbed on boot. |
| `gateway-state` | `/var/lib/private-ai-gateway` | Active upstream config, sessions, and the build/toolchain cache. |

The gateway state directory contains:

- `upstreams.json`
- `sessions.jsonl` and `sessions.jsonl.lock`
- `cache/` for Cargo, rustup, and release build output in this deployment

Do not share one state volume between concurrently running gateway processes. The session store has a single-writer lock, and the rest of the state is not a multi-replica coordination protocol.

### Seed behavior

The read-only seed is mounted at `/etc/private-ai-gateway/upstreams.seed.json` and selected by `upstream_config_seed_path`. On startup, the gateway copies it to `<state_dir>/upstreams.json` only when the active file is missing or contains only whitespace.

An existing active file always wins. Updating the seed in a later compose revision does not replace routes already stored on the persistent volume. Use the admin API for a controlled replacement. Delete the state volume only as an intentional destructive reset that also removes session and cache state.

## Configure upstreams after startup

Inspect the redacted active config:

```sh
curl --fail --silent --show-error \
  -H "Authorization: Bearer ${PRIVATE_AI_GATEWAY_ADMIN_TOKEN}" \
  http://<gateway-host>:8086/v1/admin/upstreams
```

Replace all routes atomically:

```sh
curl --fail --silent --show-error \
  -X PUT \
  -H "Authorization: Bearer ${PRIVATE_AI_GATEWAY_ADMIN_TOKEN}" \
  -H 'Content-Type: application/json' \
  --data-binary @upstreams.json \
  http://<gateway-host>:8086/v1/admin/upstreams
```

The gateway validates the complete array, writes the active file atomically, swaps runtime routing state, and starts verification prewarm. The admin response redacts provider credentials.

Supported provider values are:

- `openai-compatible`
- `anthropic`
- `aci-service`
- `tinfoil`
- `near-ai`
- `chutes`
- `secret-ai`
- `phala-direct`

## Bind public TLS identities

TLS termination sits outside the gateway, but the gateway can include the terminator's leaf-certificate SPKI in its attested workload keyset. Mount each leaf certificate and add it to the static config:

```json
{
  "tls": {
    "domain_certificates": [
      {
        "domain": "api.example.com",
        "certificate_path": "/run/certs/api.pem"
      }
    ]
  }
}
```

Preserve the original HTTP `Host` when proxying to port `8086`. The canonical `GET /v1/aci/attestation` handler selects the matching domain binding. When domain bindings are configured, an unknown or malformed host receives `404` instead of a report for another identity.

The verifier must also compare that reported SPKI with the certificate served to the client. Merely listing a certificate file in the workload does not prove that an external terminator uses it.

## Verify the deployment

Before accepting inference, a relying party should check:

| Layer | Required comparison |
| --- | --- |
| Compose | Exact services, image digests, mounts, ports, configs, and secret references match reviewed policy. |
| Launcher | `REPO_URL` and the full `COMMIT_SHA` identify reviewed gateway source. |
| Gateway static config | Bind address, state paths, dstack endpoint, TLS bindings, admin posture, and middleware settings match policy. |
| Initial upstream seed | Routes, credentials delivery, provider types, model mappings, verification pins, and refresh settings match policy. |
| Hardware report | Quote, freshness, nonce, `report_data`, event log, measured compose, and key custody satisfy the verifier profile. |
| TLS | The client-observed leaf SPKI matches the selected report binding. |
| Request | The request explicitly requires ACI verification or arrives on a reviewed TEE-only route. |

Use [Verify an attested inference](../docs/attested-confidential-inference.md) for the artifact flow. The legacy `/v1/attestation/report` endpoint is retained for compatibility; new deployment verification should use `/v1/aci/attestation`.

## Toolchain trust

`entrypoint.sh` builds `private-ai-gateway` in release mode with `cargo build --release --locked`. If Cargo is absent, it installs `rustup` through the runtime Ubuntu package repositories and resolves the current stable Rust toolchain.

That bootstrap is a development-grade trust path: the runtime archive metadata, rustup distribution, resolved stable compiler, and fetched crates participate in the effective build. A production gateway-owned image should pin and attest the compiler and dependencies, or contain a reviewed prebuilt binary. The locked Cargo dependency graph prevents resolver drift but does not by itself make the runtime toolchain reproducible.
