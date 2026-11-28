#!/bin/sh
# Stop and clean up the per-user backend before a Linux package is removed.
# Package managers invoke this as root, while the backend and its agent state
# belong to each user. Failures are logged and do not block package removal.
set -u

cleanup=0
case "${1:-}" in
  remove)
    # dpkg identifies a Conflicts/Replaces removal in the prerm arguments.
    case "${2:-}:${DPKG_MAINTSCRIPT_PACKAGE:-}:${3:-}" in
      in-favour:private-ai-proxy:private-ai-proxy-cli|in-favour:private-ai-proxy-cli:private-ai-proxy) ;;
      *) cleanup=1 ;;
    esac
    ;;
  0) cleanup=1 ;;
  failed-upgrade|upgrade|install|1|2) ;;
  *)
    # Arch passes the installed package version to pre_remove. RPM and DEB
    # have already been handled above by their numeric/action arguments.
    case "${1:-}" in
      *.*) cleanup=1 ;;
      *) ;;
    esac
    ;;
esac

pap=/usr/bin/private-ai-proxy

run_for_user() {
  set -- env HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
    XDG_RUNTIME_DIR="/run/user/$uid" DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" \
    "$pap" --yes "$@"
  if [ "$user" != root ]; then
    set -- runuser --user "$user" -- "$@"
  fi
  timeout --kill-after=5s 30s "$@" </dev/null >/dev/null 2>&1
}

if [ "$cleanup" -eq 1 ] && [ -x "$pap" ] && ! command -v timeout >/dev/null 2>&1; then
  printf '%s\n' "private-ai-proxy: timeout is unavailable, so user cleanup was skipped during package removal" >&2
elif [ "$cleanup" -eq 1 ] && [ -x "$pap" ]; then
  getent passwd 2>/dev/null | while IFS=: read -r user _ uid _ _ home _; do
    case "$uid" in
      ''|*[!0-9]*) continue ;;
    esac
    [ "$user" = root ] || [ "$uid" -ge 1000 ] || continue
    case "$home" in
      /*) [ -d "$home" ] || continue ;;
      *) continue ;;
    esac
    # Avoid creating state for users who never used Private AI Proxy. The
    # default Linux data and settings paths cover the supported layout.
    [ -d "$home/.local/share/org.dstack.private-ai-proxy" ] \
      || [ -d "$home/.config/private-ai-proxy" ] || continue
    if [ "$user" != root ] && ! command -v runuser >/dev/null 2>&1; then
      printf '%s\n' "private-ai-proxy: runuser is unavailable, so $user cleanup was skipped during package removal" >&2
      continue
    fi
    run_for_user service disable --for-uninstall ||
      printf '%s\n' "private-ai-proxy: could not disable the $user user service during package removal" >&2
    run_for_user service stop ||
      printf '%s\n' "private-ai-proxy: could not stop the $user user backend during package removal" >&2
    run_for_user stop --offline ||
      printf '%s\n' "private-ai-proxy: could not restore $user agent settings during package removal" >&2
  done
fi
