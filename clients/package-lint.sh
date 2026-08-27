#!/usr/bin/env bash
set -euo pipefail

clients_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$clients_root"

npm run lint:package -w @phala/aci-verifier
npm run build

for directory in \
  provider \
  pi-provider/packages/pi-provider-aci \
  pi-provider/packages/pi-provider-redpill \
  pi-provider/packages/pi-provider-phala-cloud \
  opencode-provider/packages/opencode-provider-aci \
  opencode-provider/packages/opencode-provider-redpill \
  opencode-provider/packages/opencode-provider-phala-cloud
do
  publint "$directory"
  attw --pack --profile esm-only "$directory"
done
