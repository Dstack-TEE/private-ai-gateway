#!/usr/bin/env bash
set -euo pipefail

repository="Dstack-TEE/private-ai-gateway"
environment="mac-app-store"
api_key=""
api_issuer=""
api_private_key=""
assume_yes=false
files=()

usage() {
  cat <<'EOF'
Usage:
  configure-app-store-secrets.sh [options] <application.p12> <installer.p12> <profile.provisionprofile>

Options:
  --repo OWNER/REPO                 GitHub repository (default: Dstack-TEE/private-ai-gateway)
  --environment NAME                GitHub environment (default: mac-app-store)
  --api-key ID                      App Store Connect API key ID
  --api-issuer UUID                 App Store Connect API issuer ID
  --api-private-key FILE            App Store Connect API private key (.p8)
  --yes                             Skip the confirmation prompt
  -h, --help                        Show this help

The script prompts for both .p12 passwords without echoing them. App Store
Connect options are optional for a signed package build with upload disabled,
but must be supplied together when configuring uploads.
EOF
}

fail() {
  printf 'Error: %s\n' "$*" >&2
  exit 1
}

require_value() {
  [[ $# -ge 2 ]] || fail "$1 requires a value"
  [[ -n ${2:-} ]] || fail "$1 requires a value"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      require_value "$@"
      repository="$2"
      shift 2
      ;;
    --environment)
      require_value "$@"
      environment="$2"
      shift 2
      ;;
    --api-key)
      require_value "$@"
      api_key="$2"
      shift 2
      ;;
    --api-issuer)
      require_value "$@"
      api_issuer="$2"
      shift 2
      ;;
    --api-private-key)
      require_value "$@"
      api_private_key="$2"
      shift 2
      ;;
    --yes)
      assume_yes=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      files+=("$@")
      break
      ;;
    -*)
      fail "unknown option: $1"
      ;;
    *)
      files+=("$1")
      shift
      ;;
  esac
done

[[ ${OSTYPE:-} == darwin* ]] || fail "run this script on macOS"
[[ ${#files[@]} -eq 3 ]] || { usage >&2; exit 2; }
[[ $repository =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || fail "invalid GitHub repository: $repository"
[[ -n $environment ]] || fail "GitHub environment cannot be empty"

application_p12=${files[0]}
installer_p12=${files[1]}
profile=${files[2]}

for command in gh openssl security base64 tr sed grep head stty; do
  command -v "$command" >/dev/null || fail "required command is unavailable: $command"
done
for file in "$application_p12" "$installer_p12" "$profile"; do
  [[ -f $file ]] || fail "file does not exist: $file"
  [[ -r $file ]] || fail "file is not readable: $file"
done

api_values=0
[[ -n $api_key ]] && ((api_values += 1))
[[ -n $api_issuer ]] && ((api_values += 1))
[[ -n $api_private_key ]] && ((api_values += 1))
[[ $api_values -eq 0 || $api_values -eq 3 ]] || fail "--api-key, --api-issuer and --api-private-key must be supplied together"
if [[ $api_values -eq 3 ]]; then
  [[ $api_key =~ ^[A-Z0-9]{10}$ ]] || fail "invalid App Store Connect API key ID"
  [[ $api_issuer =~ ^[0-9A-Fa-f]{8}-([0-9A-Fa-f]{4}-){3}[0-9A-Fa-f]{12}$ ]] || fail "invalid App Store Connect issuer ID"
  [[ -f $api_private_key && -r $api_private_key ]] || fail "API private key is not a readable file: $api_private_key"
  grep -q -- '-----BEGIN PRIVATE KEY-----' "$api_private_key" || fail "API private key is not PEM encoded"
  grep -q -- '-----END PRIVATE KEY-----' "$api_private_key" || fail "API private key is not PEM encoded"
fi

gh auth status --hostname github.com >/dev/null
gh api "repos/$repository/environments/$environment" >/dev/null || fail "GitHub environment does not exist: $environment"
security cms -D -i "$profile" >/dev/null 2>&1 || fail "provisioning profile cannot be decoded: $profile"

restore_terminal() {
  stty echo 2>/dev/null || true
}
trap 'restore_terminal; unset application_password installer_password' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

read_password() {
  local variable_name=$1
  local prompt=$2
  local value
  printf '%s' "$prompt" >&2
  stty -echo
  IFS= read -r value || { restore_terminal; fail "cannot read password"; }
  restore_terminal
  printf '\n' >&2
  [[ -n $value ]] || fail "password cannot be empty"
  printf -v "$variable_name" '%s' "$value"
}

certificate_identity() {
  local file=$1
  local password=$2
  local certificate
  local identity
  local private_key_marker
  certificate=$(printf '%s\n' "$password" | openssl pkcs12 -in "$file" -clcerts -nokeys -passin stdin 2>/dev/null) \
    || fail "cannot open PKCS#12 file or password is incorrect: $file"
  certificate=$(printf '%s\n' "$certificate" | sed -n '/-----BEGIN CERTIFICATE-----/,/-----END CERTIFICATE-----/p')
  [[ -n $certificate ]] || fail "PKCS#12 file does not contain an application certificate: $file"
  identity=$(printf '%s\n' "$certificate" \
    | openssl x509 -noout -subject -nameopt sep_multiline 2>/dev/null \
    | sed -n 's/^[[:space:]]*CN[[:space:]]*=[[:space:]]*//p' \
    | head -n 1)
  [[ -n $identity ]] || fail "cannot read certificate common name: $file"
  private_key_marker=$(printf '%s\n' "$password" \
    | openssl pkcs12 -in "$file" -nocerts -nodes -passin stdin 2>/dev/null \
    | sed -n '/-----BEGIN .*PRIVATE KEY-----/p') \
    || fail "cannot read PKCS#12 private key: $file"
  [[ -n $private_key_marker ]] || fail "PKCS#12 file does not contain a private key: $file"
  printf '%s' "$identity"
}

[[ -t 0 ]] || fail "password prompts require an interactive terminal"
application_password=""
installer_password=""
read_password application_password "Application .p12 password: "
read_password installer_password "Installer .p12 password: "

application_identity=$(certificate_identity "$application_p12" "$application_password")
installer_identity=$(certificate_identity "$installer_p12" "$installer_password")
[[ $application_identity != "$installer_identity" ]] || fail "application and installer identities must be different"

printf '\nRepository:           %s\n' "$repository"
printf 'Environment:          %s\n' "$environment"
printf 'Application identity: %s\n' "$application_identity"
printf 'Installer identity:   %s\n' "$installer_identity"
printf 'Provisioning profile: %s\n' "$profile"
printf 'ASC upload settings:  %s\n' "$([[ $api_values -eq 3 ]] && printf configured || printf skipped)"

if [[ $assume_yes != true ]]; then
  printf '\nUpload these values to GitHub? [y/N] '
  IFS= read -r confirmation
  case "$confirmation" in
    y|Y|yes|YES) ;;
    *) fail "cancelled" ;;
  esac
fi

gh variable set MAC_APP_STORE_APPLICATION_IDENTITY --repo "$repository" --env "$environment" --body "$application_identity"
gh variable set MAC_APP_STORE_INSTALLER_IDENTITY --repo "$repository" --env "$environment" --body "$installer_identity"

base64 < "$application_p12" | tr -d '\n' \
  | gh secret set MAC_APP_STORE_APPLICATION_CERTIFICATE --repo "$repository" --env "$environment"
printf '%s' "$application_password" \
  | gh secret set MAC_APP_STORE_APPLICATION_CERTIFICATE_PASSWORD --repo "$repository" --env "$environment"
base64 < "$installer_p12" | tr -d '\n' \
  | gh secret set MAC_APP_STORE_INSTALLER_CERTIFICATE --repo "$repository" --env "$environment"
printf '%s' "$installer_password" \
  | gh secret set MAC_APP_STORE_INSTALLER_CERTIFICATE_PASSWORD --repo "$repository" --env "$environment"
base64 < "$profile" | tr -d '\n' \
  | gh secret set MAC_APP_STORE_PROVISIONING_PROFILE --repo "$repository" --env "$environment"

if [[ $api_values -eq 3 ]]; then
  printf '%s' "$api_key" | gh secret set APPLE_API_KEY --repo "$repository" --env "$environment"
  printf '%s' "$api_issuer" | gh secret set APPLE_API_ISSUER --repo "$repository" --env "$environment"
  gh secret set APPLE_API_PRIVATE_KEY --repo "$repository" --env "$environment" < "$api_private_key"
fi

unset application_password installer_password
restore_terminal
trap - EXIT INT TERM

printf '\nConfigured Mac App Store signing in GitHub environment %s.\n' "$environment"
printf 'Review names with:\n'
printf '  gh variable list --repo %q --env %q\n' "$repository" "$environment"
printf '  gh secret list --repo %q --env %q\n' "$repository" "$environment"
