#!/bin/sh
set -eu

install_root=${PRIVATE_AI_PROXY_INSTALL_ROOT:-}
proc_root=${PRIVATE_AI_PROXY_PROC_ROOT:-/proc}

# In-app updates run the package manager below the desktop process.
is_installer_ancestor() {
  candidate=$1
  ancestor=$$
  while [ "$ancestor" -gt 1 ] 2>/dev/null; do
    [ "$candidate" != "$ancestor" ] || return 0
    stat=$(cat "$proc_root/$ancestor/stat" 2>/dev/null) || break
    fields=${stat##*) }
    set -- $fields
    ancestor=${2:-}
    case "$ancestor" in
      ''|*[!0-9]*) break ;;
    esac
  done
  return 1
}

pap="$install_root/usr/bin/private-ai-proxy"
[ -e "$pap" ] || [ -L "$pap" ] || exit 0
directory=$(dirname "$(readlink -f "$pap")")
for binary in private-ai-proxy-desktop private-ai-proxy-service private-ai-proxy private-ai-proxy-helper; do
  executable="$directory/$binary"
  [ -e "$executable" ] || continue
  for process in "$proc_root"/[0-9]*/exe; do
    if [ "$(readlink "$process" 2>/dev/null || true)" = "$executable" ]; then
      pid=${process#"$proc_root"/}
      pid=${pid%/exe}
      is_installer_ancestor "$pid" && continue
      echo "Private AI Proxy is still running. Close the desktop app and, as the signed-in user, run: $pap --yes service stop" >&2
      exit 1
    fi
  done
done
