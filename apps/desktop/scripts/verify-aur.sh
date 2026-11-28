#!/usr/bin/env bash
set -euo pipefail

# Runs inside archlinux:base-devel. makepkg must run as an unprivileged user.
pacman -Syu --needed --noconfirm
useradd --create-home --uid "$1" builder
printf '%s\n' 'builder ALL=(root) NOPASSWD: /usr/bin/pacman' > /etc/sudoers.d/builder
chmod 0440 /etc/sudoers.d/builder
runuser -u builder -- gpg --batch --import /repo/apps/desktop/packaging/linux-signing-key.asc
for directory in /packages/*/; do
  (
    cd "$directory"
    runuser -u builder -- makepkg --printsrcinfo > .SRCINFO
    runuser -u builder -- makepkg --syncdeps --noconfirm
  )
done
