#!/bin/sh
set -eu

# Check both names: older releases shipped pap as the executable.
for cli in /usr/bin/private-ai-proxy /usr/bin/pap; do
  [ -e "$cli" ] || [ -L "$cli" ] || continue
  owner=$(rpm -qf --qf '%{NAME}\n' "$cli" 2>/dev/null || true)
  if [ -z "$owner" ] || [ "$owner" != "@PACKAGE_NAME@" ]; then
    echo "Refusing to replace unrelated $cli${owner:+ owned by $owner}." >&2
    exit 1
  fi
  directory=$(dirname "$(readlink -f "$cli")")
  for binary in private-ai-proxy-service private-ai-proxy private-ai-proxy-helper pap-service pap; do
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
