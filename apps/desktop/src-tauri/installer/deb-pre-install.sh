#!/bin/sh
set -eu

# Check ownership of the canonical command and its aliases.
for cli in /usr/bin/private-ai-proxy /usr/bin/pap /usr/bin/aci; do
  [ -e "$cli" ] || [ -L "$cli" ] || continue
  owner=$(dpkg-query -S "$cli" 2>/dev/null | sed -n '1s/: .*//p')
  if [ -z "$owner" ] || [ "$owner" != "${DPKG_MAINTSCRIPT_PACKAGE:-}" ]; then
    echo "Refusing to replace unrelated $cli${owner:+ owned by $owner}." >&2
    exit 1
  fi
  directory=$(dirname "$(readlink -f "$cli")")
  for binary in private-ai-proxy-service private-ai-proxy private-ai-proxy-helper; do
    executable="$directory/$binary"
    [ -e "$executable" ] || continue
    for process in /proc/[0-9]*/exe; do
      [ "$(readlink "$process" 2>/dev/null || true)" != "$executable" ] || {
        echo "Private AI Proxy is still running. As the signed-in user, run: $cli --yes service stop" >&2
        exit 1
      }
    done
  done
done
