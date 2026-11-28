#!/usr/bin/env bash
set -euo pipefail
umask 077

public_key="$1"
: "${LINUX_SIGNING_KEY:?Set LINUX_SIGNING_KEY in the desktop-release environment}"
if [[ ! -s "$public_key" ]] || ! head -n 1 "$public_key" | grep -qxF -- '-----BEGIN PGP PUBLIC KEY BLOCK-----'; then
  echo "Missing or invalid Linux release public key: $public_key" >&2
  exit 1
fi

signing="${RUNNER_TEMP:?}/pap-linux-signing"
gnupg="$signing/gnupg"
mkdir "$signing" "$gnupg"
printf '%s\n' "$LINUX_SIGNING_KEY" > "$signing/key.asc"
unset LINUX_SIGNING_KEY

gpg_command=(gpg --batch --no-tty --homedir "$gnupg")
"${gpg_command[@]}" --import "$signing/key.asc"
# In GnuPG's colon output, # is an offline secret key and + is a local one.
fingerprint="$("${gpg_command[@]}" --with-colons --list-secret-keys |
  awk -F: '
    $1 == "sec" {
      if (++primary != 1 || $3 != 4096 || $4 != 1 || $12 !~ /c/ || $12 ~ /s/ || $15 != "#") invalid = 1
    }
    $1 == "ssb" {
      signing = ($12 ~ /s/ && $15 == "+")
      if (signing && ($3 != 4096 || $4 != 1)) invalid = 1
    }
    $1 == "fpr" && signing { fingerprint = $10; signing = 0; count++ }
    END { if (!invalid && primary == 1 && count == 1) print fingerprint }
  ')"
if [[ ! "$fingerprint" =~ ^[0-9A-F]{40}$ ]]; then
  echo "LINUX_SIGNING_KEY must be an OpenPGP v4 RSA-4096 signing subkey export with the primary key kept offline" >&2
  exit 1
fi

# Verify the signing subkey against the checked-in public key before packaging.
cp -- "$public_key" "$signing/archive-keyring.asc"
"${gpg_command[@]}" --output "$signing/archive-keyring.gpg" --dearmor "$signing/archive-keyring.asc"
"${gpg_command[@]}" --pinentry-mode loopback --passphrase '' --local-user "$fingerprint!" \
  --output "$signing/check.sig" --detach-sign /dev/null
gpgv --homedir "$gnupg" --keyring "$signing/archive-keyring.gpg" "$signing/check.sig" /dev/null

# Only paths and the public subkey fingerprint go into the workflow environment.
printf 'PAP_LINUX_SIGNING_KEY_FILE=%s\n' "$signing/key.asc"
printf 'PAP_LINUX_SIGNING_PUBLIC_KEY=%s\n' "$signing/archive-keyring.asc"
printf 'PAP_LINUX_SIGNING_KEYRING=%s\n' "$signing/archive-keyring.gpg"
printf 'PAP_LINUX_SIGNING_FINGERPRINT=%s\n' "$fingerprint"
