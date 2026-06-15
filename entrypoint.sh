#!/usr/bin/env bash
# entrypoint.sh: private-ai-gateway's repo-owned entry point.
#
# Ownership boundary
#   This script is owned by private-ai-gateway. It is not a launcher
#   feature, a launcher contract, or part of the launcher image's
#   responsibility. The launcher only knows three things about us: it cd's
#   into the configured repo/subdir after checking out the pinned commit, it
#   preserves the container environment, and it `exec bash entrypoint.sh`.
#   From there, *everything* - install, build, run - lives here and is
#   covered by source provenance of the pinned commit. The launcher stays
#   generic and build-system agnostic; how this aggregator gets its Rust
#   toolchain is our concern, not the launcher's.
#
# What it does
#   1. If `cargo` is not on PATH, this aggregator chooses to bootstrap a
#      Rust toolchain via apt + rustup. That choice is internal to the
#      aggregator (see "Aggregator-owned trust surface" below) and would
#      become dead code under a Rust-capable aggregator image.
#   2. Builds in release mode with --locked so Cargo.lock is authoritative
#      (a rebuild that would require resolver-level changes is a hard
#      failure, not silent dependency drift).
#   3. exec's the freshly built binary so it becomes the process the TEE
#      keeps running. The binary itself enforces all of ACI's fail-closed
#      policy (refuses test-only keys, upstream verification, etc.);
#      this script does not duplicate that logic.
#
# What it deliberately does NOT do
#   * Set the aggregator's upstream URL, identity subject, or any
#     trust-bearing policy. Runtime policy flows in through the static gateway
#     config selected by Docker Compose `environment:`, so a verifier audits it
#     as deployment config, not as bytes inside this script. Secrets should
#     come from encrypted secrets, KMS, or mounted secret files.
#   * Fall back if any step fails. Build / install / exec failure is a
#     hard exit.
#
# Aggregator-owned trust surface in this slice
#   This slice chooses runtime apt + rustup bootstrap so the aggregator
#   can run on top of the stock launcher image unchanged. That choice
#   brings three things into this aggregator's trust surface that a
#   pre-built Rust-capable aggregator image would not:
#
#     * the Ubuntu archive index that apt-get fetches at deploy time;
#     * the rustup CDN / signature key shipped by the rustup package;
#     * whichever rustc the upstream stable channel resolved to at build
#       time.
#
#   The production fix is a Rust-capable aggregator image - owned by
#   this repo, not by the launcher - that pre-installs rustc/cargo and
#   pre-populates the crate cache. When that image exists, the
#   `if ! command -v cargo` block becomes dead code; everything else in
#   this script is unchanged. See deploy/README.md for the recipe.

set -euo pipefail

PROG=entrypoint.sh
log() { printf '[%s] %s\n' "$PROG" "$*" >&2; }
die() { printf '[%s] error: %s\n' "$PROG" "$*" >&2; exit 1; }

require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "required tool not found in PATH: $1"
}

# The launcher cd's into the selected repo root or subdir before exec'ing us,
# so $PWD already equals that target. We still anchor BUILD_DIR to the
# script's own location so an accidental cd later does not move the build.
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null && pwd)
cd "$SCRIPT_DIR"

# The launcher scrubs WORK_DIR on every boot. Keep mutable Rust bootstrap and
# build state in the gateway state volume instead of the measured source
# checkout. These directories are an optimization only; source + Cargo.lock
# remain authoritative.
export PRIVATE_AI_GATEWAY_CACHE_DIR=${PRIVATE_AI_GATEWAY_CACHE_DIR:-/var/lib/private-ai-gateway/cache}
export HOME=${HOME:-/root}
export CARGO_HOME=${CARGO_HOME:-$PRIVATE_AI_GATEWAY_CACHE_DIR/cargo}
export RUSTUP_HOME=${RUSTUP_HOME:-$PRIVATE_AI_GATEWAY_CACHE_DIR/rustup}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PRIVATE_AI_GATEWAY_CACHE_DIR/target}
mkdir -p "$CARGO_HOME" "$RUSTUP_HOME" "$CARGO_TARGET_DIR"
export PATH="$CARGO_HOME/bin:$PATH"

# Aggregator-internal bootstrap: if no Rust toolchain is present in the
# image we are running on, this aggregator installs one. The launcher is
# build-system agnostic and does not care what language we are written
# in; supplying our own toolchain is our responsibility.
if ! command -v cargo >/dev/null 2>&1; then
  log "cargo not on PATH; aggregator bootstrapping a Rust toolchain via apt + rustup."
  log "WARNING: dev-grade trust path. The Ubuntu archive index, the rustup"
  log "package, and the upstream Rust stable channel are part of this"
  log "aggregator's trust surface in this build-in-TEE configuration."
  log "Production should publish a Rust-capable aggregator image so the"
  log "toolchain is covered by an aggregator-owned image digest instead."
  log "See deploy/README.md."

  require_tool apt-get

  # Apt index was wiped by the launcher image's RUN ... apt-get clean step,
  # so we re-fetch. Fail loudly if the network refuses the metadata.
  apt-get update -qq

  # The rustup package on Ubuntu 24.04 lives in universe; the stock Ubuntu
  # 24.04 docker base enables main+restricted+universe+multiverse by default,
  # so this just works. If the deploy customised sources.list and removed
  # universe, the apt-get below fails loud and we exit before building.
  apt-get install -y --no-install-recommends ca-certificates rustup

  # Install a current stable. --no-self-update so rustup does not silently
  # upgrade itself at runtime; --profile minimal keeps the install to
  # rustc + cargo + std.
  rustup toolchain install stable --no-self-update --profile minimal
  rustup default stable
fi

require_tool cargo
require_tool rustc

log "rustc: $(rustc --version)"
log "cargo: $(cargo --version)"
log "build dir: $SCRIPT_DIR"
log "cache dir: $PRIVATE_AI_GATEWAY_CACHE_DIR"

# --locked: refuse to update Cargo.lock. We want the exact dependency set the
# pinned commit ships with, not whatever the registry happens to resolve to
# at deploy time.
# --frozen would additionally refuse network access; we leave that off so the
# first build inside a fresh TEE can fetch crates. Once a base image with a
# pre-warmed crate cache lands, switching to --frozen is a one-line change.
log "cargo build --release --locked --bin private-ai-gateway"
cargo build --release --locked --bin private-ai-gateway

BIN="$CARGO_TARGET_DIR/release/private-ai-gateway"
[[ -x $BIN ]] || die "release binary not found at $BIN after build"

log "exec $BIN"
exec "$BIN"
