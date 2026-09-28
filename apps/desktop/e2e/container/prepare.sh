#!/usr/bin/env bash
# Runs as root in a fresh ubuntu:24.04 container: system packages, Node.js,
# the Private AI Proxy package under test, and the unprivileged test user.
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
# build-essential: Hermes builds native Node modules (node-pty).
apt-get install -y -qq --no-install-recommends \
  build-essential ca-certificates curl git procps xz-utils >/dev/null

node_tarball="node-v${NODE_VERSION}-linux-x64.tar.xz"
cd /tmp
curl -fsSLO "https://nodejs.org/dist/v${NODE_VERSION}/${node_tarball}"
curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/SHASUMS256.txt" |
  grep " ${node_tarball}\$" | sha256sum --check --strict --quiet -
tar -xJf "$node_tarball" -C /usr/local --strip-components=1
rm "$node_tarball"

apt-get install -y -qq /pkg/*.deb >/dev/null
useradd --create-home --shell /bin/bash tester
echo "Installed $(pap --version), Node.js $(node --version)"
