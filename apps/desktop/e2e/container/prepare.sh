#!/usr/bin/env bash
# Runs as root in a fresh ubuntu:24.04 container: system packages, Node.js,
# the Private AI Proxy package under test, and the unprivileged test user.
set -euo pipefail

# Retries a refused connection, a failed one, or a download that stalls below
# 100 KB/s for 30 seconds, instead of waiting on it indefinitely. apt already
# retries three times with a 30-second timeout by default.
download=(curl -fsSL --retry 3 --retry-connrefused --connect-timeout 20
  --speed-limit 102400 --speed-time 30)

echo "Installing system packages"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
# build-essential: Hermes builds native Node modules (node-pty).
apt-get install -y -qq --no-install-recommends \
  build-essential ca-certificates curl git procps xz-utils >/dev/null

echo "Installing Node.js $NODE_VERSION"
node_tarball="node-v${NODE_VERSION}-linux-x64.tar.xz"
cd /tmp
"${download[@]}" -O "https://nodejs.org/dist/v${NODE_VERSION}/${node_tarball}"
"${download[@]}" -O "https://nodejs.org/dist/v${NODE_VERSION}/SHASUMS256.txt"
grep " ${node_tarball}\$" SHASUMS256.txt | sha256sum --check --strict --quiet -
tar -xJf "$node_tarball" -C /usr/local --strip-components=1
rm "$node_tarball" SHASUMS256.txt

echo "Installing the package under test"
apt-get install -y -qq /pkg/*.deb >/dev/null
useradd --create-home --shell /bin/bash tester
echo "Installed $(pap --version), Node.js $(node --version)"
