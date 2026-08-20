#!/bin/sh

set -eu

repository_root="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
test_directory="$(mktemp -d)"
cleanup() {
  rm -rf "$test_directory"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

fake_bin="$test_directory/bin"
payload="$test_directory/payload"
checksum="$test_directory/checksum"
install_directory="$test_directory/install"
mkdir -p "$fake_bin"
printf '%s\n' 'fake aci release binary' >"$payload"

asset="aci-x86_64-unknown-linux-musl"
digest="$(sha256sum "$payload" | awk '{print $1}')"
printf '%s  %s\n' "$digest" "$asset" >"$checksum"

cat >"$fake_bin/curl" <<'EOF'
#!/bin/sh
set -eu
output=''
url=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output)
      output="$2"
      shift 2
      ;;
    *)
      url="$1"
      shift
      ;;
  esac
done
case "$url" in
  *.sha256)
    cp "$ACI_INSTALLER_TEST_CHECKSUM" "$output"
    ;;
  *)
    cp "$ACI_INSTALLER_TEST_PAYLOAD" "$output"
    ;;
esac
EOF
chmod +x "$fake_bin/curl"

PATH="$fake_bin:$PATH" \
  ACI_INSTALLER_TEST_PAYLOAD="$payload" \
  ACI_INSTALLER_TEST_CHECKSUM="$checksum" \
  ACI_INSTALL_DIR="$install_directory" \
  ACI_VERSION=v0.1.0 \
  sh "$repository_root/install-aci.sh"

cmp "$payload" "$install_directory/aci"
test -x "$install_directory/aci"

printf '%064d  %s\n' 0 "$asset" >"$checksum"
bad_install_directory="$test_directory/bad-install"
if PATH="$fake_bin:$PATH" \
  ACI_INSTALLER_TEST_PAYLOAD="$payload" \
  ACI_INSTALLER_TEST_CHECKSUM="$checksum" \
  ACI_INSTALL_DIR="$bad_install_directory" \
  ACI_VERSION=v0.1.0 \
  sh "$repository_root/install-aci.sh"; then
  echo "installer accepted a mismatched checksum" >&2
  exit 1
fi
test ! -e "$bad_install_directory/aci"

echo "aci installer tests passed"
