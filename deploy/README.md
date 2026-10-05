# Deploy with git-launcher

This directory contains the reference dstack deployment for Private AI Gateway. It uses the versioned [`git-launcher`](https://github.com/Dstack-TEE/dstack-examples/tree/git-launcher-v0.3.0/git-launcher) source to check out an exact gateway commit, then runs the repository-owned `entrypoint.sh` inside the confidential VM.

The checked-in manifest is an auditable starting point, not a complete
production platform. Authentication, rate limiting, TLS termination, secret
delivery, monitoring, backup, and availability remain deployment
responsibilities.

## Deployment model

The default [`compose.yaml`](compose.yaml) runs one gateway process in direct-upstream mode:

```text
client -> TLS terminator (inside attested workload) -> gateway :8086 -> configured providers
```

The gateway serves HTTP on port `8086`; it does not terminate TLS. The
checked-in compose does not run a TLS terminator. Add one before exposing an
inference endpoint, as described in
[Bind public TLS identities](#bind-public-tls-identities).

Configure the optional in-process middleware and its external HTTP control
plane in the static config when policy-based routing is required.

The compose pins this launcher image by digest:

```text
docker.io/dstacktee/git-launcher@sha256:4437dce18ec713b0991d34bd926d324966b1a0b90fad485b8ddb3f4ed2af138b
```

That digest comes from the
[`git-launcher-v0.3.0` release](https://github.com/Dstack-TEE/dstack-examples/releases/tag/git-launcher-v0.3.0).
Its
[`VERIFY.md`](https://github.com/Dstack-TEE/dstack-examples/blob/git-launcher-v0.3.0/git-launcher/VERIFY.md)
documents the Sigstore provenance and deployment-verification chain.

Review the launcher image, gateway commit, complete compose content, and runtime policy together. The launcher image alone does not identify the deployed workload.

## Prepare the deployment

Replace `COMMIT_SHA` in the `gateway-pin` config of `compose.yaml` with the full commit hash (40 or 64 hex characters) of the gateway revision you reviewed. Keep it a literal value. dstack measures the compose file before variable substitution, so a commit supplied through a variable would not be part of the measured `compose_hash`, and verifiers could not tell which code runs. Because the commit is a literal in the measured manifest, the compose hash identifies the gateway source.

Generate a strong admin token. The admin token authorizes runtime upstream replacement. The compose passes it as the `PRIVATE_AI_GATEWAY_ADMIN_TOKEN` variable, so it stays out of the measured manifest. Deliver every other secret the same way; see [Secrets and measured compose](../docs/configuration-reference.md#secrets-and-measured-compose).

The checked-in upstream seed is empty. Before deployment, either:

- replace the `gateway-upstreams` content in `compose.yaml` with reviewed routes; or
- keep it empty and use `PUT /v1/admin/upstreams` after the process starts.

[`upstreams.example.json`](upstreams.example.json) demonstrates current Anthropic, Tinfoil, NEAR AI, Chutes, and Phala-direct entries. It contains placeholders, not production policy.

## One-Command Deploy

From this directory:

```sh
phala deploy -n private-ai-gateway -c compose.yaml \
  -e PRIVATE_AI_GATEWAY_ADMIN_TOKEN=<long-random-admin-token>
```

For a development deployment, [`gateway.env.example`](gateway.env.example) shows the required variable. Passing individual encrypted variables is preferable for a production deployment because it avoids a plaintext secrets file.

Wait for the process to build and start, then check liveness:

```sh
curl --fail http://<gateway-host>:8086/health
```

Liveness only proves that the process is serving requests. It does not prove provider availability or successful attestation.

## Ownership boundary

The launcher is build-system agnostic. It parses rather than sources its
config, requires a full commit ID, checks out that commit in detached mode,
resets and cleans the checkout, and verifies that `HEAD` equals `COMMIT_SHA`.
It then preserves the container environment and runs `bash entrypoint.sh`
from the pinned checkout. It does not fall back to a branch, tag, short hash,
or another commit. The compose does not set `REPO_SUBDIR` because the gateway
entrypoint is at the repository root.

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
| `gateway-checkout` | `/var/lib/git-launcher` | Launcher-owned source checkout. Every boot runs `git reset --hard <commit>` and `git clean -ffdx`, then verifies `HEAD`, so never keep state or build output here. |
| `gateway-state` | `/var/lib/private-ai-gateway` | The gateway's `state_dir`, plus `cache/` for Cargo, rustup, and release build output. |

The gateway owns the files in its state directory; see
[Runtime state files](../docs/configuration-reference.md#runtime-state-files).
Give each running gateway process its own state volume.

### Seed behavior

The read-only seed is mounted at `/etc/private-ai-gateway/upstreams.seed.json` and selected by `upstream_config_seed_path`. The gateway copies it only when the active file is empty, as described in [Seed behavior](../docs/configuration-reference.md#seed-behavior). A seed change in a later compose revision therefore does not replace routes already stored on the persistent volume.

Use the admin API for a controlled replacement. Delete the state volume only as an intentional destructive reset that also removes session and cache state.

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

[Admin API](../docs/configuration-reference.md#admin-api) defines the response
and validation, and [Provider values](../docs/configuration-reference.md#provider-values)
lists the supported `provider` types.

## Bind public TLS identities

The gateway serves plain HTTP. Run the TLS terminator as a service in the same
compose, so it is part of the measured workload, and keep its private key
inside the TEE. Certificate issuance, renewal, and SNI routing are the
terminator's job.

The gateway can include each leaf certificate's SPKI in its attested workload
keyset. One gateway listener serves every public hostname: mount each
hostname's leaf certificate and add one entry per hostname to the static
config. This example serves two domains:

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

The terminator forwards both hostnames to port `8086` with the original HTTP
`Host` intact. A client that fetches `/v1/aci/attestation` through
`chat.example.com` gets the `chat.example.com` certificate's SPKI as the
report's `downstream_tls_binding`.
[Downstream TLS binding](../docs/configuration-reference.md#downstream-tls-binding)
defines the fields and how unknown hosts are handled.

Clients compare the reported SPKI with the certificate they are served, and
`pap` does this in its channel check. Listing a certificate in the keyset does
not prove where its private key lives, and no attestation check covers the TLS
private key. Verifiers establish its custody by reviewing the compose: which
terminator image runs, how it obtains or generates the key, and that the key
never leaves the workload. [The privacy claim](../docs/attested-confidential-inference.md#the-privacy-claim)
explains why this review is part of the trust decision.

## Verify the deployment

Before accepting inference, a relying party should check:

| Layer | Required comparison |
| --- | --- |
| Compose | Exact services, image digests, mounts, ports, configs, and secret references match reviewed policy. |
| Launcher image | The attested digest matches the reviewed release, and its Sigstore provenance identifies the expected `Dstack-TEE/dstack-examples` workflow, ref, and commit. |
| Gateway source | `REPO_URL` and the literal `COMMIT_SHA` in the measured `gateway-pin` config identify reviewed gateway source. |
| Gateway static config | Bind address, state paths, dstack endpoint, TLS bindings, admin posture, and middleware settings match policy. |
| Initial upstream seed | Routes, credentials delivery, provider types, model mappings, verification pins, and refresh settings match policy. |
| Hardware report | Quote, freshness, nonce, `report_data`, event log, measured compose, and key custody satisfy the verifier profile. |
| TLS | The client-observed leaf SPKI matches the selected report binding. |
| Request | The request explicitly requires ACI verification or arrives on a reviewed TEE-only route. |

Read `REPO_URL` and `COMMIT_SHA` from the `gateway-pin` config inside the
published `app_compose`, after hashing `app_compose` and matching it to the
pre-`system-ready` `compose-hash` event replayed into RTMR3. Do not rely on the
report's `source_provenance` field; it is the gateway's own statement (see
[Source provenance](../docs/configuration-reference.md#source-provenance)). A
matching compose hash proves the manifest was measured, not that its images,
source, compiler, or dependencies are acceptable. Approving those is the review
step.

Use [Verification and security model](../docs/attested-confidential-inference.md) for the artifact flow. Verify new deployments through `/v1/aci/attestation`; the [legacy endpoints](../docs/api-reference.md#legacy-compatibility-endpoints) exist only for older clients.

## Toolchain trust

`entrypoint.sh` builds `private-ai-gateway` in release mode with `cargo build --release --locked`. If Cargo is absent, it installs `rustup` through the runtime Ubuntu package repositories and resolves the current stable Rust toolchain.

That bootstrap is a development-grade trust path: the runtime archive metadata, rustup distribution, resolved stable compiler, and fetched crates participate in the effective build. A production gateway-owned image should pin and attest the compiler and dependencies, or contain a reviewed prebuilt binary. The locked Cargo dependency graph prevents resolver drift but does not by itself make the runtime toolchain reproducible.
