#!/usr/bin/env bash
set -euo pipefail
umask 077

packages="$1"
known_hosts="$(realpath "$2")"
: "${AUR_SSH_PRIVATE_KEY:?Set AUR_SSH_PRIVATE_KEY to the AUR account SSH key}"
scratch="$(mktemp -d)"
trap 'rm -rf -- "$scratch"' EXIT
printf '%s\n' "$AUR_SSH_PRIVATE_KEY" > "$scratch/key"
unset AUR_SSH_PRIVATE_KEY
printf -v GIT_SSH_COMMAND 'ssh -i %q -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=%q' "$scratch/key" "$known_hosts"
export GIT_SSH_COMMAND
package_version() {
  awk '$1 == "pkgver" && $2 == "=" { version = $3 } $1 == "pkgrel" && $2 == "=" { release = $3 } END { if (version && release) print version "-" release }' "$1"
}

for directory in "$packages"/*/; do
  name="$(basename "$directory")"
  [[ "$name" =~ ^[a-z0-9-]+-bin$ ]]
  checkout="$scratch/$name"
  git clone -- "ssh://aur@aur.archlinux.org/$name.git" "$checkout"
  version="$(package_version "$directory/.SRCINFO")"
  [[ -n "$version" ]]
  if [[ -f "$checkout/.SRCINFO" ]]; then
    published="$(package_version "$checkout/.SRCINFO")"
    [[ -n "$published" ]]
    if [[ "$published" == "$(printf '%s\n' "$published" "$version" | sort -V | tail -n 1)" ]]; then
      echo "$name already has version $published"
      continue
    fi
  fi
  cp -- "$directory/"{PKGBUILD,.SRCINFO,LICENSE,preremove.install} "$checkout/"
  git -C "$checkout" add -- PKGBUILD .SRCINFO LICENSE preremove.install
  if git -C "$checkout" diff --cached --quiet; then
    echo "$name is already up to date"
    continue
  fi
  git -C "$checkout" -c user.name='github-actions[bot]' -c user.email='41898282+github-actions[bot]@users.noreply.github.com' \
    commit -m "Update $name to $version"
  git -C "$checkout" push origin HEAD:master
done
