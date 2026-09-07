#!/bin/sh
set -eu

pag=/usr/bin/pag
[ -e "$pag" ] || [ -L "$pag" ] || exit 0
directory=$(dirname "$(readlink -f "$pag")")
for binary in pag-service aci private-ai-gateway-helper; do
  executable="$directory/$binary"
  [ -e "$executable" ] || continue
  for process in /proc/[0-9]*/exe; do
    [ "$(readlink "$process" 2>/dev/null || true)" != "$executable" ] || {
      echo "Private AI Gateway is still running. As the signed-in user, run: pag --yes service stop" >&2
      exit 1
    }
  done
done
