#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

packs="$scratch/packs"
consumer="$scratch/consumer"
mkdir -p "$packs" "$consumer"

for package in \
  @phala/aci-verifier \
  @phala/aci-provider \
  @phala/pi-provider-aci \
  pi-provider-redpill \
  pi-provider-phala-cloud \
  @phala/opencode-provider-aci \
  opencode-provider-redpill \
  opencode-provider-phala-cloud
do
  (cd "$repo_root/clients" && \
    npm pack --workspace "$package" --pack-destination "$packs")
done

for archive in "$packs"/*.tgz; do
  tar -tzf "$archive" package/LICENSE >/dev/null
done

cd "$consumer"
npm init --yes --silent >/dev/null
npm install --ignore-scripts --no-audit --no-fund "$packs"/*.tgz
node --input-type=module <<'NODE'
await import('@phala/aci-verifier');
await import('@phala/aci-verifier/browser');
await import('@phala/aci-verifier/node');
await import('@phala/aci-verifier/bun');
await import('@phala/aci-verifier/runtime');
await import('@phala/aci-provider');

const runtimeEntry = import.meta.resolve('@phala/aci-verifier/runtime');
if (!runtimeEntry.endsWith('/dist/node/index.js')) {
  throw new Error(`Node selected the wrong runtime entry: ${runtimeEntry}`);
}

for (const name of [
  '@phala/pi-provider-aci',
  'pi-provider-redpill',
  'pi-provider-phala-cloud',
]) {
  const entry = await import(name);
  if (typeof entry.default !== 'function') {
    throw new Error(`${name} does not export a Pi extension factory`);
  }
}

for (const name of [
  '@phala/opencode-provider-aci',
  'opencode-provider-redpill',
  'opencode-provider-phala-cloud',
]) {
  const entry = await import(name);
  if (typeof entry.default?.server !== 'function') {
    throw new Error(`${name} does not export an OpenCode v1 server plugin`);
  }
}
NODE

bun --eval '
const runtimeEntry = import.meta.resolve("@phala/aci-verifier/runtime");
if (!runtimeEntry.endsWith("/dist/bun/index.js")) {
  throw new Error(`Bun selected the wrong runtime entry: ${runtimeEntry}`);
}
await import("@phala/aci-verifier/runtime");
await import("@phala/aci-verifier/bun");
await import("@phala/aci-provider");
for (const name of [
  "@phala/opencode-provider-aci",
  "opencode-provider-redpill",
  "opencode-provider-phala-cloud",
]) {
  const entry = await import(name);
  if (typeof entry.default?.server !== "function") {
    throw new Error(`${name} does not export an OpenCode v1 server plugin`);
  }
}
'
