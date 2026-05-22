# Deploying Private AI Gateway With git-launcher

This directory contains the one-file dstack compose path for launching
Private AI Gateway through
[`git-launcher`](https://github.com/Dstack-TEE/dstack-examples/tree/main/git-launcher).

The launcher fetches a pinned `private-ai-gateway` commit, verifies `HEAD`,
scrubs the checkout, preserves the container environment, and runs the gateway
repo's own [`../entrypoint.sh`](../entrypoint.sh). The launcher remains
generic; install, build, run, and ACI policy live in this repo.

The checked-in compose runs in no-middleware mode: the public ACI frontend and
verified-provider backend are the same process, and traffic is forwarded
directly from frontend to backend. The binary can also run with a plaintext
HTTP-over-UDS middleware slot by setting
`PRIVATE_AI_GATEWAY_MIDDLEWARE_UDS_PATH` and
`PRIVATE_AI_GATEWAY_BACKEND_UDS_PATH`; this compose does not yet include a
middleware container or shared socket volume. See
[`../docs/frontend-middleware-backend.md`](../docs/frontend-middleware-backend.md)
and
[`../docs/middleware-integration.md`](../docs/middleware-integration.md).

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
PRIVATE_AI_GATEWAY_REPO_COMMIT=<full-40-hex-sha> \
PRIVATE_AI_GATEWAY_ADMIN_TOKEN=<long-random-admin-token> \
phala-h4xuser deploy -n private-ai-gateway -c compose.yaml
```

For local/dev deploys, you can also copy
[`gateway.env.example`](./gateway.env.example), export those values from your
shell, and run the same `phala-h4xuser deploy` command. For production, pass
secrets such as admin tokens through the deployment secret mechanism rather
than keeping them in a plaintext env file.

`compose.yaml` inlines the launcher config, non-secret runtime environment, and
initial upstream config. dstack therefore measures the whole launch policy into
`compose_hash`.
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
3. Preserve the container environment.
4. Run `bash entrypoint.sh` from the pinned repo.

Everything after step 4 is gateway-owned:

| Concern | Owner | Location |
| --- | --- | --- |
| Workload source pin | Launcher config | `gateway-pin` in `compose.yaml` |
| Non-secret runtime env | Deployment compose | service `environment:` in `compose.yaml` |
| Initial upstream config | Deployment compose | `gateway-upstreams` in `compose.yaml` |
| Toolchain bootstrap | Gateway repo | `../entrypoint.sh` |
| Build and exec | Gateway repo | `../entrypoint.sh` |
| Downstream ACI frontend | Gateway binary | `../src` |
| Verified-provider backend | Gateway binary | `../src` |
| Optional routing middleware | Gateway deployment | Runtime-supported; not wired in this compose yet |

The public gateway repo root contains `entrypoint.sh`, so the launcher config
does not set `REPO_SUBDIR`.

## Volumes and Reboots

The compose uses two persistent volumes with different meanings:

| Volume | Mount | Meaning |
| --- | --- | --- |
| `gateway-checkout` | `/var/lib/git-launcher` | Source checkout cache owned by `git-launcher`. Scrubbed on every boot with `git reset --hard` and `git clean -ffdx`. |
| `gateway-state` | `/var/lib/private-ai-gateway` | Gateway-owned mutable state: upstream config, receipt/body retention, and Rust build cache. |

Do not put SQLite databases, retained bodies, uploaded files, or build artefacts
under `WORK_DIR`. The source checkout is allowed to disappear and reclone. By
default `entrypoint.sh` stores Cargo/Rustup/target state under
`PRIVATE_AI_GATEWAY_CACHE_DIR=/var/lib/private-ai-gateway/cache`, so restarts
can reuse the toolchain and crate/build cache without making the source checkout
mutable.

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

The seed and non-secret runtime env are part of the attested compose. API keys
in the seed are therefore part of the deployment input and must be handled as
secrets by the deployment environment. For production, pass secrets through
dstack encrypted secrets, KMS, or mounted secret files rather than inline
compose values.

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
| Launcher image | The image digest in the attested compose equals `sha256:4437dce18ec713b0991d34bd926d324966b1a0b90fad485b8ddb3f4ed2af138b` and verifies through the `git-launcher-v0.3.0` Sigstore provenance. |
| Launcher config | `REPO_URL` and `COMMIT_SHA` in `gateway-pin` match the audited gateway commit. |
| Runtime env | Service `environment:` matches the non-secret deployment policy, including source-provenance fields, config paths, and cache location. |
| Upstream seed | `gateway-upstreams` is the reviewed initial provider policy. |
| Gateway report | `/v1/attestation/report` binds the dstack KMS identity, ACI keyset, TLS SPKI if configured, and source provenance. |

The launcher image digest alone does not identify the workload; the compose
config is part of the trust surface.

## Toolchain Posture

The current `entrypoint.sh` can bootstrap Rust with apt + rustup inside the
TEE. That keeps the first deploy path simple, but it is a development-grade
trust surface.

The production target is a gateway-owned image that already contains the
Rust toolchain, or eventually the prebuilt gateway binary. The launcher still
does not own that toolchain; the image would be built and attested by this repo
and referenced by digest in `compose.yaml`.
