#!/bin/sh

set -eu

repository="Dstack-TEE/private-ai-gateway"
requested_release="${ACI_VERSION:-latest}"

case "$requested_release" in
  latest)
    release_path="latest/download"
    ;;
  v[0-9]*)
    case "$requested_release" in
      *[!A-Za-z0-9._-]*)
        echo "ACI_VERSION contains unsupported characters: $requested_release" >&2
        exit 1
        ;;
    esac
    release_path="download/$requested_release"
    ;;
  *)
    echo "ACI_VERSION must be 'latest' or a tag such as v0.1.0" >&2
    exit 1
    ;;
esac

operating_system="$(uname -s)"
machine_architecture="$(uname -m)"

case "$operating_system" in
  Linux)
    system_target="unknown-linux-musl"
    ;;
  Darwin)
    system_target="apple-darwin"
    ;;
  *)
    echo "aci release binaries do not support $operating_system" >&2
    exit 1
    ;;
esac

case "$machine_architecture" in
  x86_64 | amd64)
    rust_architecture="x86_64"
    ;;
  arm64 | aarch64)
    rust_architecture="aarch64"
    ;;
  *)
    echo "aci release binaries do not support architecture $machine_architecture" >&2
    exit 1
    ;;
esac

target="$rust_architecture-$system_target"
asset="aci-$target"
release_url="https://github.com/$repository/releases/$release_path"

if [ "${ACI_INSTALL_DIR+x}" = x ]; then
  install_directory="$ACI_INSTALL_DIR"
else
  : "${HOME:?HOME must be set, or set ACI_INSTALL_DIR}"
  install_directory="$HOME/.local/bin"
fi
test -n "$install_directory" || {
  echo "ACI_INSTALL_DIR must not be empty" >&2
  exit 1
}

temporary_directory="$(mktemp -d)"
cleanup() {
  rm -rf "$temporary_directory"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

curl --proto '=https' --proto-redir '=https' --tlsv1.2 \
  --fail --silent --show-error --location \
  "$release_url/$asset" \
  --output "$temporary_directory/$asset"
curl --proto '=https' --proto-redir '=https' --tlsv1.2 \
  --fail --silent --show-error --location \
  "$release_url/$asset.sha256" \
  --output "$temporary_directory/$asset.sha256"

expected_digest="$(awk 'NR == 1 {print $1}' "$temporary_directory/$asset.sha256")"
case "$expected_digest" in
  '' | *[!0-9a-fA-F]*)
    echo "release checksum is not a SHA-256 digest" >&2
    exit 1
    ;;
esac
test "${#expected_digest}" -eq 64 || {
  echo "release checksum is not a SHA-256 digest" >&2
  exit 1
}

if command -v sha256sum >/dev/null 2>&1; then
  actual_digest="$(sha256sum "$temporary_directory/$asset" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  actual_digest="$(shasum -a 256 "$temporary_directory/$asset" | awk '{print $1}')"
else
  echo "installing aci requires sha256sum or shasum" >&2
  exit 1
fi

test "$actual_digest" = "$expected_digest" || {
  echo "aci checksum verification failed" >&2
  exit 1
}

mkdir -p "$install_directory"
install -m 0755 "$temporary_directory/$asset" "$install_directory/aci"

echo "installed aci to $install_directory/aci"
case ":${PATH}:" in
  *:"$install_directory":*)
    ;;
  *)
    echo "add $install_directory to PATH to run aci" >&2
    ;;
esac
