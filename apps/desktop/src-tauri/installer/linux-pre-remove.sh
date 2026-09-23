#!/bin/sh
set -eu

pap=/usr/bin/private-ai-proxy
[ -e "$pap" ] || [ -L "$pap" ] || exit 0
directory=$(dirname "$(readlink -f "$pap")")
for binary in private-ai-proxy-desktop private-ai-proxy-service private-ai-proxy private-ai-proxy-helper; do
  executable="$directory/$binary"
  [ -e "$executable" ] || continue
  for process in /proc/[0-9]*/exe; do
    [ "$(readlink "$process" 2>/dev/null || true)" != "$executable" ] || {
      echo "Private AI Proxy is still running. Close the desktop app and, as the signed-in user, run: $pap --yes service stop" >&2
      exit 1
    }
  done
done
