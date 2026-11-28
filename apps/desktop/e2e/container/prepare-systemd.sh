#!/usr/bin/env bash
set -euo pipefail

row="${PAP_E2E_MATRIX:?PAP_E2E_MATRIX is required}"
download=(curl -fsSL --retry 3 --retry-connrefused --connect-timeout 20 --speed-limit 102400 --speed-time 30)

case "$row" in
  fedora-rpm-bun)
    dnf -y -q install ca-certificates curl dbus git jq procps-ng shadow-utils tar gzip xz unzip util-linux >/dev/null
    dnf -y -q install /pkg/private-ai-proxy-cli-*.rpm >/dev/null
    ;;
  arch-pacman-opencode)
    pacman -Syu --noconfirm --needed ca-certificates curl dbus diffutils git jq procps-ng shadow tar gzip xz unzip util-linux opencode >/dev/null
    pacman -U --noconfirm /pkg/private-ai-proxy-cli-*.pkg.tar.zst >/dev/null
    ;;
  ubuntu-deb-opencode)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends ca-certificates curl dbus git jq procps unzip xz-utils tar gzip util-linux >/dev/null
    apt-get install -y -qq /pkg/private-ai-proxy-cli-*.deb >/dev/null
    ;;
  ubuntu-script-npm|ubuntu-npm-nvm)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends ca-certificates curl dbus git jq procps unzip xz-utils tar gzip util-linux build-essential >/dev/null
    ;;
esac

version="${PAP_E2E_RELEASE_TAG#desktop-v}"
channel=stable
case "$version" in *-beta.*) channel=beta ;; esac

node_tarball="node-v${NODE_VERSION}-linux-x64.tar.xz"
cd /root
"${download[@]}" -o "$node_tarball" "https://nodejs.org/dist/v${NODE_VERSION}/$node_tarball"
"${download[@]}" -o SHASUMS256.txt "https://nodejs.org/dist/v${NODE_VERSION}/SHASUMS256.txt"
grep " $node_tarball$" SHASUMS256.txt | sha256sum --check --strict --quiet -
tar -xJf "$node_tarball" -C /usr/local --strip-components=1
rm -f "$node_tarball" SHASUMS256.txt

useradd --create-home --shell /bin/bash tester
if ! passwd --delete tester >/dev/null 2>&1; then
  echo "Could not clear the test user's password" >&2
fi
usermod --password '!*' tester
printf 'tester:pap-e2e\n' | chpasswd
dotfile_snapshot=/home/tester/.pap-e2e-dotfiles-before
mkdir -p "$dotfile_snapshot"
for dotfile in .bashrc .profile .bash_profile; do
  if [[ -e "/home/tester/$dotfile" ]]; then
    cp -a "/home/tester/$dotfile" "$dotfile_snapshot/"
  else
    touch "$dotfile_snapshot/$dotfile.missing"
  fi
done
chown -R tester:tester "$dotfile_snapshot"
loginctl enable-linger tester

case "$row" in
  ubuntu-script-npm)
    # The installer runs the extracted CLI to verify its version. /tmp is noexec.
    runuser -u tester -- env HOME=/home/tester TMPDIR=/var/tmp PATH=/home/tester/.local/bin:/usr/local/bin:/usr/bin:/bin \
      bash -ec '
        installer="$(mktemp)"
        trap '\''rm "$installer"'\'' EXIT
        curl -fsSL -o "$installer" https://redpill.ai/private-ai-proxy/install.sh
        sh "$installer" --channel "$1" --version "$2" --install-dir "$HOME/.local/share/private-ai-proxy-custom" --bin-dir "$HOME/.local/bin"
      ' _ "$channel" "$version"
    ;;
  ubuntu-npm-nvm)
    runuser -u tester -- env HOME=/home/tester PATH=/usr/local/bin:/usr/bin:/bin PROFILE=/dev/null \
      bash -c 'curl -fsSL https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.3/install.sh | bash'
    runuser -l tester -c "export NODE_VERSION='$NODE_VERSION' PAP_E2E_RELEASE_TAG='$PAP_E2E_RELEASE_TAG'; . \"\$HOME/.nvm/nvm.sh\" && nvm install \"\$NODE_VERSION\" && npm install --global \"private-ai-proxy@\${PAP_E2E_RELEASE_TAG#desktop-v}\""
    ;;
esac

install_opencode() {
  export HOME=/home/tester
  export PATH=/home/tester/.local/bin:/home/tester/.npm-global/bin:/home/tester/.opencode/bin:/home/tester/.bun/bin:/usr/local/bin:/usr/bin:/bin
  case "$PAP_E2E_MATRIX" in
    ubuntu-deb-opencode|ubuntu-script-npm)
      npm config set prefix "$HOME/.npm-global"
      PAP_E2E_AGENTS=opencode /e2e/container/install-agents.sh
      ;;
    fedora-rpm-bun)
      curl -fsSL https://bun.sh/install | BUN_INSTALL="$HOME/.bun" bash
      export PATH="$HOME/.bun/bin:$PATH"
      bun add --global "opencode-ai@${OPENCODE_VERSION}"
      opencode --version
      ;;
    ubuntu-npm-nvm)
      . "$HOME/.nvm/nvm.sh"
      nvm use "$NODE_VERSION"
      npm install --global "opencode-ai@${OPENCODE_VERSION}"
      opencode --version
      ;;
    arch-pacman-opencode)
      opencode --version
      ;;
  esac
}
runuser -u tester -- env HOME=/home/tester PATH=/home/tester/.local/bin:/home/tester/.npm-global/bin:/home/tester/.opencode/bin:/home/tester/.bun/bin:/usr/local/bin:/usr/bin:/bin \
  bash -c "$(declare -f install_opencode); install_opencode"

tester_uid="$(id -u tester)"
install -d -o tester -g tester -m 700 "/run/user/$tester_uid"
if ! systemctl start systemd-logind.service; then
  echo "systemd-logind did not start; continuing with lingering user manager" >&2
fi
systemctl start "user@$tester_uid.service"
