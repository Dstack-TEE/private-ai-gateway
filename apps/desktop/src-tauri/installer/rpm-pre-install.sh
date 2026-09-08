#!/bin/sh
set -eu

pap=/usr/bin/pap
if [ ! -e "$pap" ] && [ ! -L "$pap" ]; then
  exit 0
fi

owner=$(rpm -qf --qf '%{NAME}\n' "$pap" 2>/dev/null || true)
if [ "$owner" != "@PACKAGE_NAME@" ]; then
  echo "Refusing to replace unrelated $pap${owner:+ owned by $owner}." >&2
  exit 1
fi

directory=$(dirname "$(readlink -f "$pap")")
for binary in pap-service pap private-ai-proxy-helper; do
  executable="$directory/$binary"
  [ -e "$executable" ] || continue
  for process in /proc/[0-9]*/exe; do
    [ "$(readlink "$process" 2>/dev/null || true)" != "$executable" ] || {
      echo "Private AI Proxy is still running. As the signed-in user, run: pap --yes service stop" >&2
      exit 1
    }
  done
done
