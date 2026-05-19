# Deploying private-ai-gateway under trusted-workload-launcher

This directory shows how the [`trusted-workload-launcher`][launcher]
launches the aggregator. The launcher fetches a pinned commit, verifies
`HEAD` after checkout, then runs an aggregator-owned entry script
([`../entrypoint.sh`](../entrypoint.sh)). Everything that script implies
(install, build, run) is **owned by this aggregator**, not by the
launcher.

## Required launcher-side follow-up — entry-script name

The current `trusted-workload-launcher` hardcodes the legacy default
entry-script name. In its bash source
(`bin/trusted-workload-launcher`, around line 278):

```bash
# Default mode: the workload repo provides its own entry script. We pass it
# to 'bash' rather than exec it directly so the exec bit is not required;
# the bytes are pinned by COMMIT_SHA and covered by source provenance.
local entry=tee-launch.sh
[[ -f $entry ]] || die "default entry point '$entry' not found in $target; ..."
```

This aggregator's entry script is named `entrypoint.sh`, the better
generic name and the one we want the launcher to converge on. Until the
launcher is updated, **this aggregator does not run end-to-end on the
unmodified launcher in default mode**: the launcher will `die` looking
for the legacy `tee-launch.sh` inside `REPO_SUBDIR`. The required launcher-side
change is roughly:

1. Change the hardcoded default to `entrypoint.sh`, OR make the
   default-mode entry-script name a configurable launcher-config key
   (e.g. an optional `ENTRYPOINT_SCRIPT`) defaulting to `entrypoint.sh`.
2. Update the launcher README so the contract documents `entrypoint.sh`
   (or the configurable key).
3. Optionally accept the legacy name `tee-launch.sh` as a fallback for
   one release for the benefit of existing workload repos.

This follow-up lives in the launcher repository, not here. **Do not
work around it by setting `RUN_CMD="bash entrypoint.sh"` in the launcher
config** — that pulls aggregator-owned install/run logic back into
trust-bearing launcher config, which is exactly the boundary we keep out
of (see "Ownership boundary" below).

## Ownership boundary

The launcher is generic and build-system agnostic. It does not know that
this aggregator is written in Rust, that it needs `cargo`, or that the
binary must be built before being exec'd. Its job is exactly three things:

1. Check out `REPO_URL` at `COMMIT_SHA` into `WORK_DIR`.
2. `cd $WORK_DIR/$REPO_SUBDIR`.
3. Export the `CHILD_ENV_FILE` and run our aggregator-owned entry script
   (`entrypoint.sh`, once the launcher rename above lands).

Everything past step 3 is the aggregator's responsibility. Specifically:

| Concern | Owner | Lives in |
| --- | --- | --- |
| Pinning the workload commit | Launcher config | `aggregator.conf` (this dir) |
| Forwarding deployment policy | Launcher config | `CHILD_ENV_FILE` → `aggregator.env` |
| Installing a Rust toolchain | **Aggregator** | `../entrypoint.sh` (this slice), or a Rust-capable aggregator image (production) |
| Building the binary | **Aggregator** | `../entrypoint.sh` |
| Enforcing ACI fail-closed policy | **Aggregator binary** | `../src` |

The launcher config stays small: `REPO_URL`, `COMMIT_SHA`, `REPO_SUBDIR`,
`WORK_DIR`, `CHILD_ENV_FILE`. There is no `INSTALL_CMD` and no `RUN_CMD`.
If a future aggregator stops using Rust, or moves to a pre-built binary,
or switches to a different runtime entirely, **only `entrypoint.sh`
changes** — the launcher contract is unchanged.

## Files

| Path | Role |
| --- | --- |
| `aggregator.conf` | Launcher config (trust-bearing pin: `REPO_URL`, `COMMIT_SHA`, `REPO_SUBDIR`, plus `WORK_DIR` and a pointer to the runtime env file). |
| `aggregator.env` | Runtime policy passed to `entrypoint.sh` via `CHILD_ENV_FILE` (bind address, upstream config path, optional admin token, source-provenance arms surfaced in `/v1/attestation/report`). |
| `compose.yaml` | dstack compose example. Inlines both files so they participate in `compose_hash` and are covered by dstack attestation. |

## End-to-end flow

What the launcher does (generic):

1. Parses `aggregator.conf` (line-by-line, no `source`).
2. Clones `REPO_URL` (or reuses an existing checkout whose `origin` matches)
   into `WORK_DIR`.
3. `git fetch --tags --prune` then `git checkout --detach $COMMIT_SHA`,
   re-verifies `HEAD == COMMIT_SHA`.
4. `cd $WORK_DIR/$REPO_SUBDIR` → `/var/lib/.../private-ai-gateway`.
5. Exports every `KEY=VALUE` from `aggregator.env` (the `CHILD_ENV_FILE`)
   into its own environment.
6. Run our aggregator-owned entry script (`bash entrypoint.sh` — see the
   launcher-side follow-up above for the entry-name rename the launcher
   still needs).

What `entrypoint.sh` does (aggregator-owned):

1. If `cargo` is not on `PATH`, this aggregator installs a Rust toolchain
   via apt + rustup. **This is an aggregator implementation choice for the
   first slice**, not a launcher capability. Production deployments
   should replace this step by deploying a Rust-capable aggregator image
   (see below) so the toolchain is covered by an aggregator-owned image
   digest.
2. Builds with `cargo build --release --locked --bin private-ai-gateway`.
3. `exec`s the freshly built binary, which becomes the process the TEE
   keeps running.

The aggregator reads process-level policy from the environment the launcher
prepared, and upstream/provider policy from the single upstream config file.
Every env setting uses the `PRIVATE_AI_GATEWAY_*` prefix. The binary also
accepts the older `DSTACK_LLM_ROUTER_*` names as compatibility aliases, with
the `PRIVATE_AI_GATEWAY_*` value winning if both are set. The current policy variables cover
bind address, the single upstream config file path, optional admin token,
source-provenance arms, optional retained-body duration, upstream verifier
mode, upstream HTTP timeout defaults, optional client-facing TLS certificate
paths or SPKI digests, an optional dstack SDK endpoint for KMS keys and quotes,
and the optional `PRIVATE_AI_VERIFIER_DIR` verifier checkout path. Provider
credentials and lifecycle tuning live in the upstream config file.

The upstream config itself is a mutable JSON array stored at
`PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_PATH`. Missing or empty means no upstreams
are configured yet. If `PRIVATE_AI_GATEWAY_ADMIN_TOKEN` is set, an operator can
replace that one file through `PUT /v1/admin/upstreams` and inspect the redacted
active config through `GET /v1/admin/upstreams`. The upstream choice is
transparent to relying parties through `/v1/models`, request receipts, and
`upstream.verified` events; the verifier still proves each configured upstream
before traffic is forwarded when upstream verification is required. Set the
optional per-upstream `provider` field to `openai-compatible`, `aci-dcap`,
`chutes`, `tinfoil`, or `near-ai`; it defaults to `openai-compatible`.
Provider adapters are Rust implementations, not configured shell commands. A
concrete adapter may call provider-owned verifier logic, but that call is owned
by the Rust adapter rather than upstream config. The OpenAI-compatible backend
currently enforces `tls_spki_sha256` and `tls_certificate_sha256` on the actual
HTTPS connection before forwarding plaintext. Tinfoil returns a TLS SPKI
binding through the `private-ai-verifier` bridge. NEAR AI returns a TLS SPKI
binding through the same bridge when `DSTACK_VERIFIER_URL` is available. Chutes verifies
E2EE keys against TDX `report_data`, Intel DCAP status, Chutes' public
measurement profiles, and NVIDIA NRAS nonce binding using the upstream config
`bearer_token`. Chutes binds attestation to an E2EE key rather than public TLS,
so the aggregator forwards Chutes requests only through the provider E2EE
transport and refuses to use an unverified E2EE key. The Rust adapter passes the
config token to the verifier bridge internally. After startup
verification, the aggregator proactively refreshes verifier caches before
expiry without deleting the current good cache entry; when multiple upstreams
set different positive refresh intervals, the loop uses the shortest active
interval and skips upstreams whose refresh field is `0`. It also refreshes
provider session material in the background every 45 seconds by default. For Chutes,
session refresh reuses cached verified E2EE key bindings and only refills
single-use invocation nonces. The Chutes verifier samples three
`/e2e/instances` batches by default so the verified key set is wide enough for
later nonce refresh to intersect Chutes' sampled instance responses. Configure
these with `verification_refresh_seconds`, `session_refresh_seconds`,
`chutes_e2ee_api_base`, `chutes_e2ee_discovery_rounds`, and
`chutes_e2ee_discovery_interval_seconds` in the upstream config. Set either
refresh field to `0` to disable that loop for the upstream.

TLS certificate management is outside ACI and outside the aggregator's core
protocol logic. In production, mount the client-facing leaf certificate into
the aggregator container and set `PRIVATE_AI_GATEWAY_TLS_CERT_PATHS`; the
aggregator computes `sha256(SPKI)` at startup and publishes that digest in the
attested keyset. `PRIVATE_AI_GATEWAY_TLS_SPKI_SHA256` remains for manual or
test deployments, but do not set both.

## How this aggregator gets its Rust toolchain

This is an aggregator-internal decision, not a launcher feature. Two
patterns; pick one explicitly in your deployment.

### Pattern A — runtime apt + rustup bootstrap inside the aggregator script (this slice's default)

`entrypoint.sh` apt-installs `rustup` on first start and `rustup default
stable`s a current Rust toolchain. The example files here
(`compose.yaml`, `aggregator.conf`, `aggregator.env`) use this path
unchanged. It works on top of the stock launcher image with no
launcher-side changes — that is the point: the launcher does not need to
know we are Rust.

**Aggregator-owned trust surface in this pattern:** the Ubuntu archive
index, the `rustup` package, and whichever `rustc` the upstream stable
channel resolves to at build time. None of those are pinned by a content
digest and none are covered by the launcher image's attested digest.
Acceptable for development and the first runnable slice; **not** the
production posture.

### Pattern B — Rust-capable aggregator image

The production answer is to publish an **aggregator-owned** OCI image
that already contains the Rust toolchain (and, ideally, a warmed
`Cargo.lock`-resolved registry). This image is built by this repo's
CI/CD, attested by this repo's build-provenance, and tagged with this
aggregator's version. The launcher image does not change and does not
grow Cargo.

Two equivalent constructions:

1. **Aggregator image, no launcher base.** Build a small OCI image that
   bundles `bash`, `git`, `coreutils`, `ca-certificates`, plus the Rust
   toolchain, then `COPY` in a copy of `bin/trusted-workload-launcher`
   at the audited launcher commit and set it as the entrypoint. The
   aggregator image carries both pieces — the (audited) launcher script
   and the toolchain — but its digest is published by the aggregator
   repo, not the launcher repo.
2. **Aggregator image, FROM the pinned launcher image.** Use the
   launcher image as a convenient base if it remains stable; layer in
   the toolchain. The launcher image's responsibilities do not grow;
   the derived image is still an aggregator artifact.

Sketch of construction (2), if you want the launcher entrypoint
inherited verbatim:

```dockerfile
# Pin to the audited stock launcher image digest you imported.
# Doing FROM the launcher image is a convenience; the launcher does NOT
# own this Dockerfile, this aggregator does.
FROM docker.io/dstack/trusted-workload-launcher@sha256:<launcher-digest>

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates rustup \
 && rustup toolchain install stable --no-self-update --profile minimal \
 && rustup default stable \
 && rm -rf /var/lib/apt/lists/*
ENV PATH=/root/.cargo/bin:$PATH
```

Deploy that aggregator image (pinned by its own `@sha256:…`) in
`compose.yaml` instead of the stock launcher image. The
`if ! command -v cargo` block at the top of `entrypoint.sh` then
becomes dead code; remove or keep it as the "works on stock launcher
too" path.

Further hardening: pin a specific Rust version
(`rustup toolchain install 1.94.0 --no-self-update --profile minimal`),
pre-populate `$CARGO_HOME/registry` with the dependencies `Cargo.lock`
resolves to, then build with `cargo build --frozen` so no network
access is needed at deploy time. All of that lives in this repo
because the toolchain is the aggregator's concern.

A further step is to ship a **prebuilt aggregator image** — same idea,
but the image carries the compiled binary, not the source — so
`entrypoint.sh` collapses to `exec /usr/local/bin/private-ai-gateway`.
That is the cleanest production posture and is the natural follow-up
to a prebuilt aggregator image.

## Trust surface for a deployment

| Layer | What is attested | How |
| --- | --- | --- |
| Launcher image | Audited launcher bytes (generic, build-system agnostic) | OCI digest pinned in `compose.yaml`, covered by the launcher's Sigstore build-provenance attestation. |
| Launcher config | `REPO_URL`, `COMMIT_SHA`, `REPO_SUBDIR`, `WORK_DIR`, `CHILD_ENV_FILE` path | dstack `compose_hash` over the compose file that inlines this config. |
| Workload bytes | The whole `private-ai-gateway/` tree at `COMMIT_SHA`, including `entrypoint.sh` and `Cargo.lock` | Source provenance of the pinned commit; a verifier re-reads the public repo at that commit. |
| Build toolchain | Pattern A: dev-grade trust surface listed above. Pattern B: aggregator image digest. | Aggregator-owned. |
| Runtime policy | `aggregator.env` contents | dstack `compose_hash` (the env block is inlined into the compose file). |
| Active upstream config | Mutable operator policy in the single upstream config file | Not part of workload identity; exposed through `/v1/models`, redacted admin state, receipts, and upstream-verification events. |

`Cargo.lock` is checked in for exactly this reason. `entrypoint.sh`
passes `--locked` so a build that would require resolver-level dependency
changes is a hard failure, not silent dependency drift.

## Repository Name

This package is intended to live in the public
`Dstack-TEE/private-ai-gateway` repository. The product/project name is
**Private AI Gateway**. The specification it implements is **Attested
Confidential Inference**.

[launcher]: https://github.com/Dstack-TEE/dstack-examples/tree/main/trusted-workload-launcher
