#!/bin/sh
set -eu

REPOSITORY="Dstack-TEE/private-ai-gateway"
CHANNEL="stable"
VERSION=""
INSTALL_DIR="${XDG_DATA_HOME:-${HOME:?HOME is required}/.local/share}/private-ai-proxy"
BIN_DIR="${HOME}/.local/bin"

usage() {
  cat <<'USAGE'
Install the Private AI Proxy CLI for the current user.

Usage:
  install-private-ai-proxy.sh [options]

Options:
  --channel stable|beta  Release channel used when --version is omitted.
  --version VERSION      Install an exact published version.
  --install-dir PATH     Versioned installation root.
  --bin-dir PATH         Directory for private-ai-proxy, pap, and aci links.
  -h, --help             Show this help.

The installer does not require root and never edits shell startup files.
USAGE
}

fail() {
  printf 'private-ai-proxy installer: %s\n' "$*" >&2
  exit 1
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --channel)
      [ "$#" -ge 2 ] || fail "--channel requires a value"
      CHANNEL=$2
      shift 2
      ;;
    --channel=*) CHANNEL=${1#*=}; shift ;;
    --version)
      [ "$#" -ge 2 ] || fail "--version requires a value"
      VERSION=$2
      shift 2
      ;;
    --version=*) VERSION=${1#*=}; shift ;;
    --install-dir)
      [ "$#" -ge 2 ] || fail "--install-dir requires a value"
      INSTALL_DIR=$2
      shift 2
      ;;
    --install-dir=*) INSTALL_DIR=${1#*=}; shift ;;
    --bin-dir)
      [ "$#" -ge 2 ] || fail "--bin-dir requires a value"
      BIN_DIR=$2
      shift 2
      ;;
    --bin-dir=*) BIN_DIR=${1#*=}; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown option: $1" ;;
  esac
done

case "$CHANNEL" in
  stable|beta) ;;
  *) fail "channel must be stable or beta" ;;
esac
case "$INSTALL_DIR" in /*) ;; *) fail "--install-dir must be an absolute path" ;; esac
case "$BIN_DIR" in /*) ;; *) fail "--bin-dir must be an absolute path" ;; esac
command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v tar >/dev/null 2>&1 || fail "tar is required"

curl_get() {
  curl --fail --silent --show-error --location --retry 3 --connect-timeout 10 --max-time 120 --proto '=https' --tlsv1.2 "$@"
}

if [ -z "$VERSION" ]; then
  feed_url="https://github.com/$REPOSITORY/releases/download/desktop-updates-$CHANNEL/latest.json"
  feed=$(curl_get "$feed_url") || fail "cannot read the $CHANNEL release feed"
  VERSION=$(printf '%s\n' "$feed" | sed -n 's/^[[:space:]]*"version":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)
  [ -n "$VERSION" ] || fail "release feed does not contain a version"
fi

if [ "$CHANNEL" = stable ]; then
  printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || fail "version $VERSION does not match the stable channel"
else
  printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+-beta\.[1-9][0-9]*$' || fail "version $VERSION does not match the beta channel"
fi

case "$(uname -s)" in
  Darwin) PLATFORM=macos ;;
  Linux) PLATFORM=linux ;;
  *) fail "supported operating systems are macOS and Linux" ;;
esac
case "$(uname -m)" in
  arm64|aarch64) ARCH=arm64 ;;
  x86_64|amd64) ARCH=x64 ;;
  *) fail "supported architectures are arm64 and x86_64" ;;
esac

BASE="private-ai-proxy-cli-$VERSION-$PLATFORM-$ARCH"
ARCHIVE="$BASE.tar.gz"
RELEASE_URL="https://github.com/$REPOSITORY/releases/download/desktop-v$VERSION"
TMP_ROOT=${TMPDIR:-/tmp}
TEMP_DIR=$(mktemp -d "$TMP_ROOT/private-ai-proxy-install.XXXXXX") || fail "cannot create a temporary directory"
STAGE_DIR=""
cleanup() {
  if [ -n "$STAGE_DIR" ] && [ -d "$STAGE_DIR" ]; then rm -rf "$STAGE_DIR"; fi
  for command_name in private-ai-proxy pap aci; do
    temporary_link="$BIN_DIR/.$command_name.$$"
    if [ -L "$temporary_link" ]; then rm -f "$temporary_link"; fi
  done
  rm -rf "$TEMP_DIR"
}
trap cleanup EXIT HUP INT TERM

curl_get --output "$TEMP_DIR/$ARCHIVE" "$RELEASE_URL/$ARCHIVE" || fail "cannot download $ARCHIVE"
curl_get --output "$TEMP_DIR/SHA256SUMS" "$RELEASE_URL/SHA256SUMS" || fail "cannot download SHA256SUMS"

EXPECTED_SHA=$(awk -v name="$ARCHIVE" '$2 == name { print $1 }' "$TEMP_DIR/SHA256SUMS")
[ "$(printf '%s\n' "$EXPECTED_SHA" | grep -c .)" -eq 1 ] || fail "SHA256SUMS must contain exactly one entry for $ARCHIVE"
if command -v sha256sum >/dev/null 2>&1; then
  ACTUAL_SHA=$(sha256sum "$TEMP_DIR/$ARCHIVE" | awk '{ print $1 }')
elif command -v shasum >/dev/null 2>&1; then
  ACTUAL_SHA=$(shasum -a 256 "$TEMP_DIR/$ARCHIVE" | awk '{ print $1 }')
else
  fail "sha256sum or shasum is required"
fi
[ "$ACTUAL_SHA" = "$EXPECTED_SHA" ] || fail "checksum mismatch for $ARCHIVE"

mkdir "$TEMP_DIR/extracted"
tar -tzf "$TEMP_DIR/$ARCHIVE" | sort > "$TEMP_DIR/archive.list"
cat > "$TEMP_DIR/expected.list" <<EOF_EXPECTED
$BASE/
$BASE/aci
$BASE/pap
$BASE/private-ai-proxy
$BASE/private-ai-proxy-helper
$BASE/private-ai-proxy-service
EOF_EXPECTED
sort "$TEMP_DIR/expected.list" -o "$TEMP_DIR/expected.list"
cmp -s "$TEMP_DIR/archive.list" "$TEMP_DIR/expected.list" || fail "archive contains unexpected paths"
tar -xzf "$TEMP_DIR/$ARCHIVE" -C "$TEMP_DIR/extracted"
SOURCE_DIR="$TEMP_DIR/extracted/$BASE"
for executable in private-ai-proxy private-ai-proxy-service private-ai-proxy-helper; do
  [ -f "$SOURCE_DIR/$executable" ] && [ -x "$SOURCE_DIR/$executable" ] || fail "archive is missing executable $executable"
done
for alias in pap aci; do
  [ -L "$SOURCE_DIR/$alias" ] && [ "$(readlink "$SOURCE_DIR/$alias")" = private-ai-proxy ] || fail "archive has an invalid $alias alias"
done
[ "$("$SOURCE_DIR/private-ai-proxy" --version)" = "private-ai-proxy $VERSION" ] || fail "downloaded CLI reports an unexpected version"

VERSIONS_DIR="$INSTALL_DIR/versions"
TARGET_DIR="$VERSIONS_DIR/$VERSION"
mkdir -p "$VERSIONS_DIR" "$BIN_DIR"

old_executable=""
for command_name in private-ai-proxy pap aci; do
  command_path="$BIN_DIR/$command_name"
  if [ -e "$command_path" ] || [ -L "$command_path" ]; then
    [ -L "$command_path" ] || fail "$command_path exists and is not managed by this installer"
    link_target=$(readlink "$command_path")
    case "$link_target" in
      "$VERSIONS_DIR"/*/private-ai-proxy) ;;
      *) fail "$command_path points outside $VERSIONS_DIR" ;;
    esac
    link_version=${link_target#"$VERSIONS_DIR"/}
    link_version=${link_version%/private-ai-proxy}
    printf '%s' "$link_version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-beta\.[1-9][0-9]*)?$' || fail "$command_path has an invalid managed target"
    if [ -n "$old_executable" ] && [ "$old_executable" != "$link_target" ]; then
      fail "managed command links do not point to the same installation"
    fi
    old_executable=$link_target
  fi
done

if [ -n "$old_executable" ] && [ -x "$old_executable" ]; then
  "$old_executable" --yes service stop >/dev/null || fail "cannot stop the existing Private AI Proxy service"
fi

if [ -e "$TARGET_DIR" ]; then
  [ -d "$TARGET_DIR" ] || fail "$TARGET_DIR exists and is not a directory"
  [ "$("$TARGET_DIR/private-ai-proxy" --version 2>/dev/null || true)" = "private-ai-proxy $VERSION" ] || fail "$TARGET_DIR is not a valid $VERSION installation"
  for executable in private-ai-proxy private-ai-proxy-service private-ai-proxy-helper; do
    [ -f "$TARGET_DIR/$executable" ] && [ -x "$TARGET_DIR/$executable" ] || fail "$TARGET_DIR is missing executable $executable"
  done
else
  STAGE_DIR="$VERSIONS_DIR/.stage-$VERSION-$$"
  [ ! -e "$STAGE_DIR" ] || fail "staging directory already exists: $STAGE_DIR"
  mkdir "$STAGE_DIR"
  for executable in private-ai-proxy private-ai-proxy-service private-ai-proxy-helper; do
    cp "$SOURCE_DIR/$executable" "$STAGE_DIR/$executable"
    chmod 755 "$STAGE_DIR/$executable"
  done
  mv "$STAGE_DIR" "$TARGET_DIR"
  STAGE_DIR=""
fi

for command_name in private-ai-proxy pap aci; do
  command_path="$BIN_DIR/$command_name"
  temporary_link="$BIN_DIR/.$command_name.$$"
  ln -s "$TARGET_DIR/private-ai-proxy" "$temporary_link"
  mv -f "$temporary_link" "$command_path"
done

if [ -n "$old_executable" ]; then
  old_directory=$(dirname "$old_executable")
  case "$old_directory" in
    "$VERSIONS_DIR"/*)
      if [ "$old_directory" != "$TARGET_DIR" ]; then rm -rf "$old_directory"; fi
      ;;
  esac
fi

printf 'Installed Private AI Proxy %s\n' "$VERSION"
printf 'Commands: %s/pap, %s/private-ai-proxy, %s/aci\n' "$BIN_DIR" "$BIN_DIR" "$BIN_DIR"
case ":${PATH:-}:" in
  *:"$BIN_DIR":*) ;;
  *) printf 'Add %s to PATH, then open a new shell.\n' "$BIN_DIR" ;;
esac
