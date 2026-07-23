# Deploying Private AI Gateway With git-launcher

This directory contains the one-file dstack compose path for launching
Private AI Gateway through
[`git-launcher`](https://github.com/Dstack-TEE/dstack-examples/tree/main/git-launcher).

Two complete deployment files are provided:

| File | Services |
| --- | --- |
| `compose.yaml` | Gateway only. |
| `compose.privatemode.yaml` | Gateway plus the official digest-pinned Privatemode proxy in the same measured workload. |

The launcher fetches a pinned `private-ai-gateway` commit, verifies `HEAD`,
scrubs the checkout, preserves the container environment, and runs the gateway
repo's own [`../entrypoint.sh`](../entrypoint.sh). The launcher remains
generic; install, build, run, and ACI policy live in this repo.

The checked-in compose runs in no-middleware mode: the public ACI frontend and
verified-provider backend are the same process, and traffic is forwarded
directly from frontend to backend. To enable middleware, configure it in the
static gateway config (this compose leaves it disabled). See the
[configuration reference](../docs/configuration-reference.md#middleware).

## One-Command Deploy

The compose hard-codes the released launcher image:

```text
docker.io/dstacktee/git-launcher@sha256:4437dce18ec713b0991d34bd926d324966b1a0b90fad485b8ddb3f4ed2af138b
```

That digest comes from
[`git-launcher-v0.3.0`](https://github.com/Dstack-TEE/dstack-examples/releases/tag/git-launcher-v0.3.0).

Prepare an audited gateway commit, then run:

```bash
cd deploy
phala deploy -n private-ai-gateway -c compose.yaml \
  -e PRIVATE_AI_GATEWAY_REPO_COMMIT=<full-40-hex-sha> \
  -e PRIVATE_AI_GATEWAY_ADMIN_TOKEN=<long-random-admin-token>
```

For Privatemode, use the co-deployed variant. It requires a reviewed manifest
file and its digest; [`privatemode.env.example`](./privatemode.env.example)
lists all inputs.

```bash
export PRIVATE_AI_GATEWAY_REPO_COMMIT=<full-40-hex-sha>
export PRIVATE_AI_GATEWAY_ADMIN_TOKEN=<long-random-admin-token>
export PRIVATE_AI_GATEWAY_ADMIN_TOKEN_SHA256="$(printf %s "$PRIVATE_AI_GATEWAY_ADMIN_TOKEN" | sha256sum | cut -d' ' -f1)"
export PRIVATE_AI_GATEWAY_INFERENCE_TOKEN=<long-random-client-token>
export PRIVATE_AI_GATEWAY_INFERENCE_TOKEN_SHA256="$(printf %s "$PRIVATE_AI_GATEWAY_INFERENCE_TOKEN" | sha256sum | cut -d' ' -f1)"
export PRIVATEMODE_API_KEY=<privatemode-api-key>
export PRIVATEMODE_MANIFEST_PATH=/absolute/path/to/exact-reviewed-manifest.json

./render-privatemode-compose.sh /tmp/private-ai-gateway-privatemode.json
phala deploy -n private-ai-gateway \
  -c /tmp/private-ai-gateway-privatemode.json \
  -e PRIVATE_AI_GATEWAY_ADMIN_TOKEN="$PRIVATE_AI_GATEWAY_ADMIN_TOKEN" \
  -e PRIVATEMODE_API_KEY="$PRIVATEMODE_API_KEY"
```

Render before deployment so the exact byte-for-byte manifest, its digest, the
credential digest, admin-token digest, downstream inference-token digest,
image digest, and git commit are part of the measured Compose. The admin token
itself is deliberately absent from the rendered file and is passed through
Phala's encrypted environment instead. The downstream inference token remains
client-side and is not passed to the deployment; callers send it as the Bearer
credential on inference requests. The
Privatemode API key follows the same path into one Compose-managed secret
mounted into both services. The proxy reads it through its official
`--apiKey @<file>` interface; the gateway reads it only at startup to verify the
renderer-derived measured digest, then drops the bytes.
The renderer replaces the local manifest path with inline JSON content and
verifies that the rendered bytes retain the source file's SHA-256, including
whitespace and its final newline.
Those non-secret content pins are also service labels, ensuring Compose
recreates both services when a mounted inline config changes instead of
restarting a container with stale config bytes.

That compose pins
`ghcr.io/edgelesssys/privatemode/privatemode-proxy` at OCI digest
`sha256:ff900b263a51a437633d15da809e7893a31fa4b1f4acfa4e526c075682d84307`,
mounts the same manifest and credential secret into both services, and does not
publish the proxy port. Add only the mutable model route through the admin API
after boot; `bearer_token` is forbidden for Privatemode. The official proxy
loads one credential at startup, so rotating it requires rerendering with the
new `PRIVATEMODE_API_KEY` and redeploying the gateway and proxy together. A
gateway-only restart with a secret that does not match measured policy fails
closed.

For local/dev deploys, you can also copy
[`gateway.env.example`](./gateway.env.example), fill in its values, and pass it
as `-e gateway.env`. For production, pass variables with repeated `-e KEY=VALUE`
arguments so secrets such as admin tokens do not remain in a plaintext env file.

`compose.yaml` inlines the launcher config, the static gateway config, and the
initial upstream config. dstack measures the raw manifest into `compose_hash`,
and the published `app_compose` contains variable references rather than their
values.
After deployment, the gateway listens on port `8086`. The Privatemode variant
rejects inference requests unless `Authorization: Bearer
$PRIVATE_AI_GATEWAY_INFERENCE_TOKEN` hashes to the digest measured in its
static config. Health, attestation, transparency, and model-catalog endpoints
remain public; the admin API uses its separate admin token.

The gateway consumes two JSON files:

| File | Compose config | Runtime role |
| --- | --- | --- |
| Static gateway config | `gateway-config` | Startup policy: bind address, TLS certificate bindings, dstack endpoint, admin token, gateway state directory, and read-only seed paths. |
| Upstream seed config | `gateway-upstreams` seed copied to `<state_dir>/upstreams.json` on first boot | Initial provider/model routing policy. The live file is replaced by the admin API. |

The checked-in compose starts with an empty upstream seed:

```json
[]
```

For a real deployment, replace the `gateway-upstreams` `content:` block in
`compose.yaml` with the provider routes you want to boot with, or keep it
empty and set the config after boot through `PUT /v1/admin/upstreams`.
[`upstreams.example.json`](./upstreams.example.json) shows the current
three-provider shape.

## Ownership boundary

The launcher is build-system agnostic. It does not know this repo is Rust and
does not contain a Cargo install command. Its default-mode contract is:

1. Clone `REPO_URL`.
2. Check out exactly `COMMIT_SHA`.
3. Preserve the container environment.
4. Run `bash entrypoint.sh` from the pinned repo.

Everything after step 4 is gateway-owned:

| Concern | Owner | Location |
| --- | --- | --- |
| Workload source pin | Launcher config | `gateway-pin` in `compose.yaml` |
| Static gateway config | Deployment compose | `gateway-config` in `compose.yaml` |
| Runtime bootstrap env | Deployment compose | service `environment:` in `compose.yaml` points at the static gateway config and cache directory |
| Initial upstream config | Deployment compose | `gateway-upstreams` in `compose.yaml` |
| Toolchain bootstrap | Gateway repo | `../entrypoint.sh` |
| Build and exec | Gateway repo | `../entrypoint.sh` |
| Downstream ACI frontend | Gateway binary | `../src` |
| Verified-provider backend | Gateway binary | `../src` |
| Optional routing middleware | Gateway deployment | Router helpers exist, but this compose and static config do not wire middleware |

The public gateway repo root contains `entrypoint.sh`, so the launcher config
does not set `REPO_SUBDIR`.

## Volumes and Reboots

The compose uses two persistent volumes with different meanings:

| Volume | Mount | Meaning |
| --- | --- | --- |
| `gateway-checkout` | `/var/lib/git-launcher` | Source checkout cache owned by `git-launcher`. Scrubbed on every boot with `git reset --hard` and `git clean -ffdx`. |
| `gateway-state` | `/var/lib/private-ai-gateway` | Gateway-owned mutable state: active upstream config, attested-session log, and Rust build cache. |

Do not put gateway state or build artefacts under `WORK_DIR`. The source
checkout is allowed to disappear and reclone. By default `entrypoint.sh` stores
Cargo/Rustup/target state under
`PRIVATE_AI_GATEWAY_CACHE_DIR=/var/lib/private-ai-gateway/cache`, so restarts
can reuse the toolchain and crate/build cache without making the source checkout
mutable.

## Gateway And Upstream Config

The complete config and environment-variable reference is
[`../docs/configuration-reference.md`](../docs/configuration-reference.md).

The static gateway config is mounted read-only:

```text
/etc/private-ai-gateway/gateway.config.json
```

It is selected by the only gateway config-path env variable in the compose:

```text
PRIVATE_AI_GATEWAY_CONFIG_PATH=/etc/private-ai-gateway/gateway.config.json
```

The gateway config names the writable state directory:

```text
/var/lib/private-ai-gateway
```

Inside that directory, the gateway owns `upstreams.json` and `sessions.jsonl`.
Operators do not configure those writable file paths individually.

The compose-mounted seed is read-only:

```text
/etc/private-ai-gateway/upstreams.seed.json
```

The state directory and seed path are configured inside `gateway-config`:

```json
{
  "state_dir": "/var/lib/private-ai-gateway",
  "upstream_config_seed_path": "/etc/private-ai-gateway/upstreams.seed.json"
}
```

### Multi-Domain Listener Usage

The compose keeps a single gateway listener on port `8086`. To serve multiple
public domains, configure DNS, TLS termination, SNI routing, and reverse proxying
outside this repo, and forward each public hostname to that listener with the
original HTTP `Host` intact.

Mount the leaf certificate used for each public hostname and list those
hostnames in the static gateway config:

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

When a client requests `/v1/attestation/report` through `chat.example.com`, the
gateway selects the `chat.example.com` certificate SPKI for
`attestation.evidence.downstream_tls_binding`. A request with an unknown `Host`
returns `404 not_found` instead of an attestation report.

At startup, if `<state_dir>/upstreams.json` is missing or whitespace-only, the
gateway validates the seed and copies it into that active config file. If the
active config already contains anything, the seed is ignored and the active
config wins. This lets a single compose boot a complete initial deployment
without blocking later admin updates.

Changing `gateway-upstreams` in a later compose revision does not overwrite an
existing active config volume. Use the admin API to replace the config, or
delete the `gateway-state` volume intentionally before redeploying.

The static gateway config, seed, and bootstrap environment references are part
of the attested Compose. The canonical attestation endpoint publishes that raw
Compose preimage for independent verification. Do not place plaintext secrets
in it. Pass secrets through dstack encrypted environment variables, KMS, or
mounted secret files. Keep only variable references in Compose.

Source provenance is not set in the gateway config. The gateway reports the
`REPO_URL` and `COMMIT_SHA` observed by git-launcher. The canonical report also
returns the raw measured `app_compose`, so a verifier can hash it and match the
RTMR3-bound `compose-hash`. The checked-in manifest measures the commit variable
name, not its encrypted value. Binding the reported commit and accepted image
digests to reviewed source is intentionally left as a verifier-policy TODO.

Example seed:

```json
[
  {
    "name": "tinfoil",
    "provider": "tinfoil",
    "base_url": "https://inference.tinfoil.sh",
    "models": {
      "kimi-k2": "kimi-k2-6"
    },
    "bearer_token": "<tinfoil-api-key>"
  }
]
```

Supported provider values are `openai-compatible`, `anthropic`, `aci-service`,
`tinfoil`, `near-ai`, `chutes`, `secret-ai`, `privatemode`, and
`phala-direct`.

For `aci-service`, `base_url` is the HTTPS origin used for both model traffic and
`/v1/attestation/report`. The router fetches the report through normal TLS,
derives the attested TLS SPKI binding from that report, then pins that SPKI for
the actual upstream model request.

## Runtime Admin API

When `admin_token` is set in the static gateway config, the same active config
can be inspected and replaced:

```bash
curl -H "Authorization: Bearer $PRIVATE_AI_GATEWAY_ADMIN_TOKEN" \
  http://127.0.0.1:8086/v1/admin/upstreams

curl -X PUT \
  -H "Authorization: Bearer $PRIVATE_AI_GATEWAY_ADMIN_TOKEN" \
  -H "content-type: application/json" \
  --data-binary @upstreams.json \
  http://127.0.0.1:8086/v1/admin/upstreams
```

The admin response redacts bearer tokens and returns the active config digest.

## Verification Surface

A verifier checks:

| Layer | What to compare |
| --- | --- |
| Launcher image | The image digest in the attested compose equals `sha256:4437dce18ec713b0991d34bd926d324966b1a0b90fad485b8ddb3f4ed2af138b` and verifies through the `git-launcher-v0.3.0` Sigstore provenance. |
| Launcher config | `REPO_URL` and `COMMIT_SHA` in `gateway-pin` match the audited gateway commit. |
| Gateway config | `gateway-config` matches the reviewed startup policy, including TLS certificate bindings, state directory, dstack endpoint, admin token, and read-only input paths. |
| Runtime env | Service `environment:` points at the reviewed gateway config and cache location. |
| Upstream seed | `gateway-upstreams` is the reviewed initial provider policy. |
| Gateway report | `/v1/attestation/report` binds the dstack KMS identity, ACI keyset, TLS SPKI if configured, and the git-launcher source provenance when present. |

The launcher image digest alone does not identify the workload; the compose
config is part of the trust surface.

## Toolchain Posture

The current `entrypoint.sh` can bootstrap a missing native compiler/linker
with apt + build-essential and Rust with apt + rustup inside the TEE. That
keeps the first deploy path simple, but it is a development-grade trust
surface.

The production target is a gateway-owned image that already contains the
Rust toolchain, or eventually the prebuilt gateway binary. The launcher still
does not own that toolchain; the image would be built and attested by this repo
and referenced by digest in `compose.yaml`.
