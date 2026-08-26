#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

packs="$scratch/packs"
consumer="$scratch/consumer"
mkdir -p "$packs" "$consumer"

(cd "$repo_root/clients/verifier-ts" && npm pack --pack-destination "$packs")
for package in @phala/pi-provider-aci pi-provider-redpill pi-provider-phala-cloud; do
  (cd "$repo_root/clients/pi-provider" && \
    npm pack --workspace "$package" --pack-destination "$packs")
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
NODE

bun --eval '
const runtimeEntry = import.meta.resolve("@phala/aci-verifier/runtime");
if (!runtimeEntry.endsWith("/dist/bun/index.js")) {
  throw new Error(`Bun selected the wrong runtime entry: ${runtimeEntry}`);
}
await import("@phala/aci-verifier/runtime");
await import("@phala/aci-verifier/bun");
'
