# Shared by the desktop updater and the CLI's printed update command.
set -eu
fingerprint=$1
key_package=$2
local_key=$3
key_url=$4
mode=$5

key=
# Older RPM versions represent imported keys as virtual packages. RPM 6
# supports querying its keyring directly by full fingerprint.
if ! rpm -q "$key_package" >/dev/null 2>&1 &&
   ! rpmkeys --list "$fingerprint" >/dev/null 2>&1; then
    key=$local_key
    if [ ! -f "$key" ]; then
        if [ "$mode" = prepare ]; then
            scratch=$6
        else
            scratch=$(mktemp -d)
            trap 'rm -r -- "$scratch"' EXIT
        fi
        key="$scratch/linux-signing-key.asc"
        curl -fsSL --proto '=https' --proto-redir '=https' --max-time 30 -o "$key" -- "$key_url"
        keys=$(gpg --no-options --batch --homedir "$scratch" --with-colons --show-keys -- "$key")
        if ! printf '%s\n' "$keys" | awk -F: -v expected="$fingerprint" '
            $1 == "pub" { primary++; next_fingerprint = 1 }
            $1 == "sec" { invalid = 1 }
            $1 == "fpr" && next_fingerprint { actual = $10; next_fingerprint = 0 }
            END { exit !(primary == 1 && !invalid && actual == expected) }
        '; then
            echo "The downloaded Private AI Proxy RPM key has an unexpected primary fingerprint." >&2
            exit 1
        fi
    fi
fi

if [ "$mode" = prepare ]; then
    printf '%s' "$key"
else
    if [ -n "$key" ]; then
        sudo rpm --import "$key"
    fi
    sudo rpm -U "$6"
fi
