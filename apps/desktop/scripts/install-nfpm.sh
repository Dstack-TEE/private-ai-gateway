#!/usr/bin/env bash
set -euo pipefail

NFPM_VERSION=2.49.0
case "$(uname -m)" in
  x86_64) asset="nfpm_${NFPM_VERSION}_Linux_x86_64.tar.gz" digest=2afaa5ffb5e3493402e31e7c67a5a1fb42133e3f66fc315ca0051934125561c7 ;;
  aarch64) asset="nfpm_${NFPM_VERSION}_Linux_arm64.tar.gz" digest=d4eee71839e92c68cbd3b101640bad32d2856fc551acae0635be9118e48c9d9c ;;
  *) echo "Unsupported nfpm runner architecture $(uname -m)" >&2; exit 1 ;;
esac
directory="$RUNNER_TEMP/nfpm"
mkdir -p "$directory"
curl -fsSL -o "$directory/$asset" "https://github.com/goreleaser/nfpm/releases/download/v${NFPM_VERSION}/$asset"
printf '%s  %s\n' "$digest" "$directory/$asset" | sha256sum -c -
tar -xzf "$directory/$asset" -C "$directory" nfpm
rm "$directory/$asset"
echo "$directory" >> "$GITHUB_PATH"
