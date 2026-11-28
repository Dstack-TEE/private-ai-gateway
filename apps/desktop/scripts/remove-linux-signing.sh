#!/usr/bin/env bash
set -euo pipefail

signing="$RUNNER_TEMP/pap-linux-signing"
if [[ -d "$signing/gnupg" ]]; then
  gpgconf --homedir "$signing/gnupg" --kill gpg-agent || true
fi
rm -rf -- "$signing"
