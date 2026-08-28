#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

packs="$scratch/packs"
consumer="$scratch/consumer"
pi_npm="$scratch/pi-agent/npm"
mkdir -p "$packs" "$consumer" "$pi_npm"

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

# Pi deliberately omits host-provided peer dependencies when it installs an
# extension. Load the packaged extension with Pi's own loader in that layout.
npm init --yes --silent --prefix "$pi_npm" >/dev/null
npm install --prefix "$pi_npm" --legacy-peer-deps --ignore-scripts --no-audit --no-fund \
  "$packs"/phala-aci-verifier-*.tgz \
  "$packs"/phala-aci-provider-*.tgz \
  "$packs"/phala-pi-provider-aci-*.tgz \
  "$packs"/pi-provider-phala-cloud-*.tgz

for peer in pi-ai pi-coding-agent pi-tui; do
  if [[ -e "$pi_npm/node_modules/@earendil-works/$peer" ]]; then
    echo "Pi smoke unexpectedly installed host peer @earendil-works/$peer" >&2
    exit 1
  fi
done

PI_CODING_AGENT_DIR="$scratch/pi-agent" \
  "$repo_root/clients/node_modules/.bin/pi" \
  --offline \
  --no-context-files \
  --no-skills \
  --no-themes \
  --extension "$pi_npm/node_modules/pi-provider-phala-cloud/dist/index.js" \
  --list-models >/dev/null

smoke_opencode_provider() {
  local package_name="$1"
  local provider_id="$2"
  local config
  local models

  config="$(
    node --input-type=module - "$consumer/node_modules/$package_name" "$provider_id" <<'NODE'
import { pathToFileURL } from "node:url";

const plugin = pathToFileURL(process.argv[2]).href;
process.stdout.write(JSON.stringify({ plugin: [plugin], enabled_providers: [process.argv[3]] }));
NODE
  )"

  models="$(
    XDG_CONFIG_HOME="$scratch/opencode/$provider_id/config" \
    XDG_CACHE_HOME="$scratch/opencode/$provider_id/cache" \
    XDG_DATA_HOME="$scratch/opencode/$provider_id/data" \
    XDG_STATE_HOME="$scratch/opencode/$provider_id/state" \
    OPENCODE_CONFIG_CONTENT="$config" \
      "$repo_root/clients/node_modules/.bin/opencode" models "$provider_id"
  )"

  if ! grep -q "^$provider_id/" <<<"$models"; then
    echo "OpenCode did not load any $provider_id models" >&2
    exit 1
  fi
}

smoke_opencode_provider opencode-provider-phala-cloud phala
smoke_opencode_provider opencode-provider-redpill redpill
