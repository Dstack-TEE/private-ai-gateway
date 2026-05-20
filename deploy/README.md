# Deploying Private AI Gateway With trusted-workload-launcher

This directory contains the one-file dstack compose path for launching
Private AI Gateway through
[`trusted-workload-launcher`](https://github.com/Dstack-TEE/dstack-examples/tree/trusted-workload-launcher/trusted-workload-launcher).

The launcher fetches a pinned `private-ai-gateway` commit, verifies `HEAD`,
exports a runtime env file, and runs the gateway repo's own
[`../entrypoint.sh`](../entrypoint.sh). The launcher remains generic; install,
build, run, and ACI policy live in this repo.

## One-Command Deploy

The compose hard-codes the released launcher image:

```text
docker.io/dstacktee/trusted-workload-launcher@sha256:211d3922f21a9ec6fac252db2cc703a5d3412973509655c9f91f3036c6101afb
```

That digest comes from
[`trusted-workload-launcher-v0.1.0`](https://github.com/Dstack-TEE/dstack-examples/releases/tag/trusted-workload-launcher-v0.1.0).

Prepare an audited gateway commit, then run:

```bash
cd deploy
PRIVATE_AI_GATEWAY_REPO_COMMIT=<full-40-hex-sha> \
PRIVATE_AI_GATEWAY_ADMIN_TOKEN=<long-random-admin-token> \
phala-h4xuser deploy -n private-ai-gateway -c compose.yaml
```

You can also copy [`gateway.env.example`](./gateway.env.example), export those
values from your shell, and run the same `phala-h4xuser deploy` command.

`compose.yaml` inlines the launcher config, runtime env, and initial upstream
config. dstack therefore measures the whole launch policy into `compose_hash`.
After deployment, the gateway listens on port `8086`.

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
3. Export `CHILD_ENV_FILE`.
4. Run `bash entrypoint.sh` from the pinned repo.

Everything after step 4 is gateway-owned:

| Concern | Owner | Location |
| --- | --- | --- |
| Workload source pin | Launcher config | `gateway-pin` in `compose.yaml` |
| Runtime env | Deployment compose | `gateway-runtime` in `compose.yaml` |
| Initial upstream config | Deployment compose | `gateway-upstreams` in `compose.yaml` |
| Toolchain bootstrap | Gateway repo | `../entrypoint.sh` |
| Build and exec | Gateway repo | `../entrypoint.sh` |
| ACI verification and receipts | Gateway binary | `../src` |

The public gateway repo root contains `entrypoint.sh`, so the launcher config
does not set `REPO_SUBDIR`.

## Upstream Config Seed

The active upstream config path is mutable:

```text
/var/lib/private-ai-gateway/upstreams.json
```

The compose-mounted seed is read-only:

```text
/etc/private-ai-gateway/upstreams.seed.json
```

The runtime env variable is:

```text
PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_SEED_PATH=/etc/private-ai-gateway/upstreams.seed.json
```

At startup, if the active config path is missing or whitespace-only, the
gateway validates the seed and copies it into the active config path. If the
active config already contains anything, the seed is ignored and the active
config wins. This lets a single compose boot a complete initial deployment
without blocking later admin updates.

Changing `gateway-upstreams` in a later compose revision does not overwrite an
existing active config volume. Use the admin API to replace the config, or
delete the `gateway-state` volume intentionally before redeploying.

The seed and runtime env are part of the attested compose. API keys in the seed
are therefore part of the deployment input and must be handled as secrets by
the deployment environment.

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

Supported provider values are `openai-compatible`, `aci-dcap`, `tinfoil`,
`near-ai`, and `chutes`.

## Runtime Admin API

When `PRIVATE_AI_GATEWAY_ADMIN_TOKEN` is set, the same active config can be
inspected and replaced:

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
| Launcher image | The image digest in the attested compose equals `sha256:211d3922f21a9ec6fac252db2cc703a5d3412973509655c9f91f3036c6101afb` and verifies through the `trusted-workload-launcher-v0.1.0` Sigstore provenance. |
| Launcher config | `REPO_URL` and `COMMIT_SHA` in `gateway-pin` match the audited gateway commit. |
| Runtime env | `gateway-runtime` matches the deployment policy, including source-provenance fields and config paths. |
| Upstream seed | `gateway-upstreams` is the reviewed initial provider policy. |
| Gateway report | `/v1/attestation/report` binds the dstack KMS identity, ACI keyset, TLS SPKI if configured, and source provenance. |

The launcher image digest alone does not identify the workload; the compose
config is part of the trust surface.

## Toolchain Posture

The current `entrypoint.sh` can bootstrap Rust with apt + rustup inside the
TEE. That keeps the first deploy path simple, but it is a development-grade
trust surface.

The production target is an aggregator-owned image that already contains the
Rust toolchain, or eventually the prebuilt gateway binary. The launcher still
does not own that toolchain; the image would be built and attested by this repo
and referenced by digest in `compose.yaml`.
