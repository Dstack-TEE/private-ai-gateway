#!/bin/bash
set -euo pipefail

unsigned=false
if [[ ${1:-} == --unsigned ]]; then
  unsigned=true
  shift
fi

app=${1:?Usage: package-macos-pkg.sh [--unsigned] app output.pkg version identifier}
output=${2:?Usage: package-macos-pkg.sh [--unsigned] app output.pkg version identifier}
version=${3:?Usage: package-macos-pkg.sh [--unsigned] app output.pkg version identifier}
identifier=${4:?Usage: package-macos-pkg.sh [--unsigned] app output.pkg version identifier}
receipt_version=${version%%-*}
identity=${PAG_INSTALLER_SIGN_IDENTITY:-}
keychain=${PAG_INSTALLER_KEYCHAIN:-}
app_name=$(basename "$app")

[[ -d "$app" && "$output" = /* ]]
[[ "$app_name" =~ ^[A-Za-z0-9._\ -]+\.app$ ]]
[[ "$identifier" =~ ^[A-Za-z0-9.-]+$ ]]
[[ "$receipt_version" =~ ^[0-9]+(\.[0-9]+)*$ ]]
if [[ "$unsigned" == false ]]; then
  [[ -n "$identity" ]]
  if [[ -n "$keychain" ]]; then
    security find-identity -v "$keychain" | grep -F -- "$identity" >/dev/null
  else
    security find-identity -v | grep -F -- "$identity" >/dev/null
  fi
fi

scratch=$(mktemp -d "${TMPDIR:-/tmp}/pag-pkg.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
root="$scratch/root"
scripts="$scratch/scripts"
mkdir -p "$root/Applications" "$root/usr/local/bin" "$scripts"
ditto "$app" "$root/Applications/$app_name"
installed_macos="/Applications/$app_name/Contents/MacOS"
ln -s "$installed_macos/pag" "$root/usr/local/bin/pag"

printf '#!/bin/bash\nset -eu\napp=%q\n' "$installed_macos" > "$scripts/preinstall"
cat >> "$scripts/preinstall" <<'SCRIPT'
command=/usr/local/bin/pag
expected="$app/pag"
if [[ -e "$command" || -L "$command" ]]; then
  [[ -L "$command" && "$(readlink "$command")" == "$expected" ]] || {
    echo "Refusing to replace unrelated $command" >&2
    exit 1
  }
fi
for binary in private-ai-gateway-desktop pag pag-service aci private-ai-gateway-helper; do
  if [[ -e "$app/$binary" ]] && /usr/sbin/lsof -t -- "$app/$binary" >/dev/null 2>&1; then
    echo "Private AI Gateway is still using $app/$binary." >&2
    echo "As the signed-in user, run 'pag --yes service stop' and quit the app before installing." >&2
    exit 1
  fi
done
SCRIPT
chmod 755 "$scripts/preinstall"

component="$scratch/private-ai-gateway.pkg"
pkgbuild \
  --root "$root" \
  --scripts "$scripts" \
  --identifier "$identifier.pkg" \
  --version "$receipt_version" \
  --install-location / \
  "$component"
product_args=(--package "$component")
if [[ "$unsigned" == false ]]; then
  product_args+=(--sign "$identity")
  if [[ -n "$keychain" ]]; then
    product_args+=(--keychain "$keychain")
  fi
fi
productbuild "${product_args[@]}" "$output"
