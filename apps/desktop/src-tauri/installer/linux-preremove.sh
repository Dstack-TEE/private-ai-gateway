#!/bin/sh
# Stop and clean up the per-user backend before a Linux package is removed.
# Package managers invoke this as root, while the backend and its agent state
# belong to each user. Failures are logged and do not block package removal.
set -u

cleanup=0
case "${1:-}" in
  remove|0) cleanup=1 ;;
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
  user=$1
  uid=$2
  home=$3
  if [ "$user" = root ]; then
    env HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
      XDG_RUNTIME_DIR="/run/user/$uid" DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" \
      "$pap" --yes service disable >/dev/null 2>&1 ||
      printf '%s\n' "private-ai-proxy: could not disable the root user service during package removal" >&2
    env HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
      XDG_RUNTIME_DIR="/run/user/$uid" DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" \
      "$pap" --yes service stop >/dev/null 2>&1 ||
      printf '%s\n' "private-ai-proxy: could not stop the root user backend during package removal" >&2
    env HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
      XDG_RUNTIME_DIR="/run/user/$uid" DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" \
      "$pap" --yes stop --offline >/dev/null 2>&1 ||
      printf '%s\n' "private-ai-proxy: could not restore root user agent settings during package removal" >&2
  elif command -v runuser >/dev/null 2>&1; then
    runuser --user "$user" -- env HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
      XDG_RUNTIME_DIR="/run/user/$uid" DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" \
      "$pap" --yes service disable >/dev/null 2>&1 ||
      printf '%s\n' "private-ai-proxy: could not disable the $user user service during package removal" >&2
    runuser --user "$user" -- env HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
      XDG_RUNTIME_DIR="/run/user/$uid" DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" \
      "$pap" --yes service stop >/dev/null 2>&1 ||
      printf '%s\n' "private-ai-proxy: could not stop the $user user backend during package removal" >&2
    runuser --user "$user" -- env HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
      XDG_RUNTIME_DIR="/run/user/$uid" DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" \
      "$pap" --yes stop --offline >/dev/null 2>&1 ||
      printf '%s\n' "private-ai-proxy: could not restore $user agent settings during package removal" >&2
  else
    printf '%s\n' "private-ai-proxy: runuser is unavailable, so $user cleanup was skipped during package removal" >&2
  fi
}

if [ "$cleanup" -eq 1 ] && [ -x "$pap" ]; then
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
    run_for_user "$user" "$uid" "$home"
  done
fi
