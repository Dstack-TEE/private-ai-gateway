# Install Private AI Proxy

Private AI Proxy ships as the `pap` CLI and a desktop app. Every installation
method below installs the CLI; Homebrew and GitHub Releases also offer the
desktop app. [The CLI reference](../apps/desktop/docs/cli.md) describes `pap`
and its aliases.

The CLI runs on macOS, Windows, and Linux, on arm64 and x64. Linux needs glibc
2.35 or newer; musl distributions such as Alpine are not supported.

The steps below install the stable release. `pap curl` and the key-custody
flags (`--accept-subject` with `--accept-dstack-kms-root-public-key`) need
0.2.0 or later.

## Native install (recommended)

The official install scripts download the standalone CLI from GitHub Releases
and need no Node.js or package manager.

### macOS and Linux

Run the official installer from the stable RedPill URL:

```sh
curl -fsSL https://redpill.ai/private-ai-proxy/install.sh | sh
```

To review the script or pass options, download it first:

```sh
curl -fsSLO https://redpill.ai/private-ai-proxy/install.sh
sh install.sh
```

The script supports macOS and Linux on arm64 and x86_64. It installs without
root, verifies the release archive against `SHA256SUMS`, validates its contents,
stops the previous user backend during an upgrade, and atomically switches the
three commands. It never edits shell startup files; if `$HOME/.local/bin` is
not on `PATH`, the installer says so, and you add it yourself.

Options:

```text
--channel stable|beta
--version VERSION
--install-dir PATH
--bin-dir PATH
```

The defaults are `${XDG_DATA_HOME:-$HOME/.local/share}/private-ai-proxy` for
versioned files and `$HOME/.local/bin` for commands.

### Windows

Run the official installer from PowerShell:

```powershell
irm https://redpill.ai/private-ai-proxy/install.ps1 | iex
```

To review the script or pass parameters, download it first:

```powershell
Invoke-WebRequest https://redpill.ai/private-ai-proxy/install.ps1 -OutFile install.ps1
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

The script supports Windows x64 and ARM64. It verifies the release ZIP against
`SHA256SUMS`, validates every archive entry, stops the previous user backend,
uses the CLI's ownership-aware registration to update the user PATH, and restores
the previous installation if registration fails.

PowerShell parameters:

```text
-Channel stable|beta
-Version VERSION
-InstallDir PATH
```

The default installation directory is
`%LOCALAPPDATA%\Programs\Private AI Proxy CLI`.

### Verify and install a pinned release

Choose a `desktop-v` tag on [GitHub Releases](https://github.com/Dstack-TEE/private-ai-gateway/releases)
that lists the installer assets. Set the version below to that release's
version without the `desktop-v` prefix. For a beta such as `X.Y.Z-beta.1`, set
the channel to `beta`. Run the commands in a fresh directory with `gpg` and
`gpgv` available.

Download the installer, the signed checksum list, and the public key from the
same tag. Before continuing, confirm that the key file shows a single primary
key with fingerprint `1857 9A14 41C5 278F B2B1 A943 060C 5C65 1FC4 6140`.

#### macOS and Linux release asset

The checksum command uses `shasum`.

```sh
version='X.Y.Z'
channel=stable
repo=https://github.com/Dstack-TEE/private-ai-gateway
tag="desktop-v$version"
(
  set -e
  for file in install-private-ai-proxy.sh SHA256SUMS SHA256SUMS.asc; do
    curl -fsSLo "$file" "$repo/releases/download/$tag/$file"
  done
  curl -fsSLo linux-signing-key.asc "$repo/raw/$tag/apps/desktop/packaging/linux-signing-key.asc"
  gpg --show-keys --with-fingerprint linux-signing-key.asc
)
```

After confirming the fingerprint, verify the signature and checksum before
running the installer with the same version.

```sh
gpg --dearmor --output release-keyring.gpg linux-signing-key.asc &&
  gpgv --keyring "$PWD/release-keyring.gpg" SHA256SUMS.asc SHA256SUMS &&
  awk '$2 == "install-private-ai-proxy.sh"' SHA256SUMS | shasum -a 256 -c &&
  sh ./install-private-ai-proxy.sh --channel "$channel" --version "$version"
```

#### Windows release asset

```powershell
$ErrorActionPreference = 'Stop'
$version = 'X.Y.Z'
$channel = 'stable'
$repo = 'https://github.com/Dstack-TEE/private-ai-gateway'
$tag = "desktop-v$version"
foreach ($file in 'install-private-ai-proxy.ps1', 'SHA256SUMS', 'SHA256SUMS.asc') {
    Invoke-WebRequest -UseBasicParsing "$repo/releases/download/$tag/$file" -OutFile $file
}
Invoke-WebRequest -UseBasicParsing "$repo/raw/$tag/apps/desktop/packaging/linux-signing-key.asc" -OutFile linux-signing-key.asc
gpg --show-keys --with-fingerprint linux-signing-key.asc
```

After confirming the fingerprint, run the following in the same PowerShell session.

```powershell
gpg --dearmor --output release-keyring.gpg linux-signing-key.asc
if ($LASTEXITCODE -ne 0) { throw 'Could not create the release keyring' }
gpgv --keyring (Join-Path $PWD.Path 'release-keyring.gpg') SHA256SUMS.asc SHA256SUMS
if ($LASTEXITCODE -ne 0) { throw 'Invalid SHA256SUMS signature' }
$entries = @(Get-Content SHA256SUMS | Where-Object { $_ -cmatch '^[0-9a-f]{64}  install-private-ai-proxy\.ps1$' })
if ($entries.Count -ne 1) { throw 'Expected one installer checksum' }
$actual = (Get-FileHash -Algorithm SHA256 .\install-private-ai-proxy.ps1).Hash
if ($actual -ne $entries[0].Substring(0, 64)) { throw 'Installer checksum mismatch' }
powershell -ExecutionPolicy Bypass -File .\install-private-ai-proxy.ps1 -Channel $channel -Version $version
```

You can also verify GitHub build provenance with `gh attestation verify`.
Pass the local installer path, `--repo Dstack-TEE/private-ai-gateway`, and
`--source-ref refs/tags/desktop-vX.Y.Z` with your exact release tag.

## Windows with WinGet

Use WinGet to install the desktop app or the standalone CLI. Both packages
track stable releases.

```powershell
winget install --id DstackTEE.PrivateAIProxy --exact
# For the CLI without the desktop app:
winget install --id DstackTEE.PrivateAIProxy.CLI --exact
```

The desktop app can update from Settings > About. For the standalone CLI,
close any running proxy processes before running the upgrade command that
`pap update` prints.

## Homebrew

Install the CLI Formula, or the Cask with the desktop app for macOS, directly
from the official tap. On Homebrew 6+, each fully qualified command
automatically taps the repository and trusts only the item being installed:

```sh
brew install dstack-tee/private-ai/private-ai-proxy
brew install --cask dstack-tee/private-ai/private-ai-proxy
```

To use short names such as `brew install private-ai-proxy` or `brew upgrade
private-ai-proxy`, trust the whole tap first:

```sh
brew tap dstack-tee/private-ai
brew trust dstack-tee/private-ai
brew install private-ai-proxy
brew install --cask private-ai-proxy
```

Trust for an item installed by its fully qualified name persists, so later
upgrades continue to work. Older Homebrew versions do not have tap trust; the
fully qualified one-line commands still work there.

The Formula exposes `pap`, `private-ai-proxy`, and `aci`. Homebrew owns upgrades
and removal for both the Formula and Cask installations.

The tap carries stable releases only. It is updated by pull requests that the
tap's own workflow opens after a stable release and a maintainer merges, so it
can trail the latest GitHub release. Use the native install script or npm for beta
versions or when you need a release the tap does not have yet.

## npm

```sh
npm install --global private-ai-proxy
pap --help
```

npm installs the native executables for the current operating system and CPU
architecture as an optional dependency. Do not install with `--omit=optional`
or `--no-optional`: without that dependency, `pap` only prints the reinstall
command.

Install `private-ai-proxy` (the stable release), `private-ai-proxy@beta`, or an
exact version such as `private-ai-proxy@0.2.0`. Do not use a prerelease range
such as `private-ai-proxy@^0.2.0-beta.1`: it can resolve to a platform-specific
version (for example `0.2.0-beta.2-linux-x64`) instead of the installable
package.

npm 9.6.3 and 9.6.4, bundled with Node 20.0 and 20.1, skip the Linux native
executables even on glibc systems. Upgrade npm if `pap` reports that they are
missing.

## APT on Debian and Ubuntu

Bootstrap the repository key and verify its primary fingerprint before installing
it. The fingerprint must be `1857 9A14 41C5 278F B2B1 A943 060C 5C65 1FC4 6140`.

```sh
curl -fsSLO https://pkg.redpill.ai/private-ai-proxy-archive-keyring.gpg
gpg --show-keys --with-fingerprint private-ai-proxy-archive-keyring.gpg
```

```sh
sudo install -m 0755 -d /etc/apt/keyrings
sudo install -m 0644 private-ai-proxy-archive-keyring.gpg /etc/apt/keyrings/
curl -fsSLO https://pkg.redpill.ai/apt/private-ai-proxy.sources
sudo install -m 0644 private-ai-proxy.sources /etc/apt/sources.list.d/
sudo apt update
sudo apt install private-ai-proxy
```

For the CLI without the desktop app, install `private-ai-proxy-cli`. To opt in
to beta releases, also install the beta source.

```sh
curl -fsSLO https://pkg.redpill.ai/apt/private-ai-proxy-beta.sources
sudo install -m 0644 private-ai-proxy-beta.sources /etc/apt/sources.list.d/
sudo apt update
```

Run `pap update` for the APT upgrade command, or upgrade during your regular
system updates. The keyring in `/etc/apt/keyrings` remains available after
uninstalling the package. Repeat the key setup when the repository certificate
is rotated.

## DNF and Zypper

Verify the repository key against the same primary fingerprint shown above.
DNF may also request confirmation of the key for repository metadata.

```sh
curl -fsSLO https://pkg.redpill.ai/key.asc
gpg --show-keys --with-fingerprint key.asc
sudo rpm --import key.asc
```

On Fedora and other DNF systems:

```sh
curl -fsSLO https://pkg.redpill.ai/rpm/private-ai-proxy.repo
sudo install -m 0644 private-ai-proxy.repo /etc/yum.repos.d/
sudo dnf install private-ai-proxy
```

On openSUSE systems:

```sh
sudo zypper addrepo --refresh https://pkg.redpill.ai/rpm/private-ai-proxy.repo
sudo zypper refresh
sudo zypper install private-ai-proxy
```

For the CLI without the desktop app, install `private-ai-proxy-cli`. For
betas, enable both repositories by repeating the repository setup with
`private-ai-proxy-beta.repo`. Run `pap update` for the DNF or Zypper upgrade
command. Remove or disable the beta repository to return to stable. Your
package manager will upgrade when a newer stable version is available.

## Arch User Repository

Install [private-ai-proxy-bin](https://aur.archlinux.org/packages/private-ai-proxy-bin)
for the desktop app or [private-ai-proxy-cli-bin](https://aur.archlinux.org/packages/private-ai-proxy-cli-bin)
for the CLI. The AUR tracks stable releases. Before installing, import the
upstream OpenPGP certificate into your user's GnuPG keyring. Verify its
primary fingerprint against the value in [Verify Linux packages](#verify-linux-packages).

```sh
curl -fsSLo linux-signing-key.asc https://github.com/Dstack-TEE/private-ai-gateway/raw/main/apps/desktop/packaging/linux-signing-key.asc
gpg --show-keys --with-fingerprint linux-signing-key.asc
gpg --import linux-signing-key.asc
```

Install and upgrade the package with your usual AUR helper. You can also
follow [Arch’s manual AUR build instructions](https://wiki.archlinux.org/title/Arch_User_Repository#Installing_and_upgrading_packages)
to build the package with `makepkg`.

## Desktop app and native packages

Signed desktop app installers, portable CLI archives, and Linux DEB, RPM, and
Arch packages are published on the
[GitHub Releases](https://github.com/Dstack-TEE/private-ai-gateway/releases)
page. Native package managers own upgrades and removal for their installations.

### Verify Linux packages

Download `SHA256SUMS` and `SHA256SUMS.asc` alongside the packages. Download the
Linux public key as `linux-signing-key.asc` from the build commit linked in
the release notes. Verify that its primary fingerprint matches
`1857 9A14 41C5 278F B2B1 A943 060C 5C65 1FC4 6140` before using it.

```bash
gpg --show-keys --with-fingerprint linux-signing-key.asc
gpg --dearmor --output private-ai-proxy-archive-keyring.gpg linux-signing-key.asc
gpgv --keyring "$PWD/private-ai-proxy-archive-keyring.gpg" SHA256SUMS.asc SHA256SUMS &&
  sha256sum --ignore-missing --check SHA256SUMS
```

For an RPM, import the trusted key and verify the embedded package signature.
Set `rpm_file` to the desktop or CLI filename you downloaded.

```bash
sudo rpm --import linux-signing-key.asc
rpmkeys -Kv "$rpm_file"
```

Confirm that the signature reports `OK`, then run `sudo rpm -U "$rpm_file"`.

For an Arch package, set `arch_file` to the downloaded package basename.
Save its matching `.pkg.tar.zst.sig` as `$arch_file.sig.download` so pacman
leaves verification to `gpgv`. Use the shipped keyring. For a first install or
an upgrade from an older package without a keyring, use the keyring you created
above from the verified public certificate.

```bash
keyring=/usr/share/keyrings/private-ai-proxy-archive-keyring.gpg
if [ ! -f "$keyring" ]; then
  keyring="$PWD/private-ai-proxy-archive-keyring.gpg"
fi
gpgv --keyring "$keyring" "$arch_file.sig.download" "./$arch_file" &&
  sudo pacman -U "./$arch_file"
```

The commands use the default Arch policy that permits local installs after
external verification. Use the signed checksum list to verify downloaded
DEBs. DEBs also ship the binary keyring for APT repository metadata
verification.

## Update

| Installation | How to update |
| --- | --- |
| macOS or Linux install script | Run `pap update`. If a managed command link is missing from PATH, rerun the [installer](#macos-and-linux) with the original directories. |
| Linux DEB package | Desktop installs update from Settings > About. For CLI-only installs, run `pap update` or use `pap doctor` to see the APT commands. |
| Linux RPM package | Desktop installs update from Settings > About. For CLI-only installs, run `pap update` or use `pap doctor` to see the RPM commands. The updater [reuses the registered key, imports the local certificate, or checks the fingerprint of a downloaded certificate](../apps/desktop/docs/distribution.md#linux-release-signing) before `rpm -U`. Older desktop RPMs may need a [manual upgrade](#verify-linux-packages) first. |
| Linux Arch package | Follow Settings > About for desktop installs or run `pap update` for CLI-only installs. Download the package and detached signature, verify the signature with `gpgv` against the shipped keyring, then install the local file with `pacman -U`. For older packages without a keyring, follow [Verify Linux packages](#verify-linux-packages). |
| npm, pnpm, Bun, or Yarn Classic | Run `pap update` and follow the command for your package manager. The npm command uses the prefix where this copy is installed. On Windows, stop the backend with the first command before running the package manager to unlock the executable. |
| Homebrew | For the CLI, run `pap update` to see the formula upgrade command. For the app, run `brew upgrade --cask dstack-tee/private-ai/private-ai-proxy`. |
| Windows PowerShell installer | Run `pap update` to get a command that reuses the installation directory. For an older install in a custom directory, rerun the [installer](#windows) once with that `-InstallDir` to record the install method. |
| Desktop app | Check for updates in Settings > About. For Mac App Store installs, use the App Store. |

## Uninstall

To remove a Linux package repository after uninstalling the desktop app or CLI,
remove the source files for the channels you enabled.

- APT: Remove `/etc/apt/sources.list.d/private-ai-proxy.sources` and, if enabled,
  `/etc/apt/sources.list.d/private-ai-proxy-beta.sources`. Then remove
  `/etc/apt/keyrings/private-ai-proxy-archive-keyring.gpg` and run `sudo apt update`.
- DNF: Remove `/etc/yum.repos.d/private-ai-proxy.repo` and, if enabled,
  `/etc/yum.repos.d/private-ai-proxy-beta.repo`.
- Zypper: Run `sudo zypper removerepo private-ai-proxy` and, if enabled,
  `sudo zypper removerepo private-ai-proxy-beta`.

Before removing a CLI installation without a removal hook, quit the desktop
app if open and run the following as the user who ran Private AI Proxy. If
login supervision is unavailable, continue with the stop and offline restore.

```sh
pap --yes service disable --for-uninstall
pap --yes service stop
pap --yes stop --offline --for-uninstall
```

| Installation | How to remove it |
| --- | --- |
| macOS or Linux install script | Delete the `pap`, `aci`, and `private-ai-proxy` links from the [installer's bin directory](#macos-and-linux) and remove its versioned installation directory. |
| Linux DEB, RPM, or Arch package | Remove `private-ai-proxy-cli` or `private-ai-proxy` with the package manager. Current packages attempt the cleanup above automatically while preserving the login preference. |
| npm | Run `npm uninstall --global private-ai-proxy`. |
| Homebrew | Run `brew uninstall dstack-tee/private-ai/private-ai-proxy` for the CLI or add `--cask` for the app. |
| Windows PowerShell installer | Run `pap --yes cli uninstall` to remove its PATH registration, then delete the [CLI install directory](#windows). |
| Desktop app | On Windows, use **Settings > Apps > Installed apps**. On macOS, choose **Stop All and Quit**, turn off login supervision as described in [CLI distribution](../apps/desktop/docs/cli-distribution.md) if enabled, then move the app to Trash. |

Linux package removal continues if a cleanup step fails. Package switches may
trigger cleanup if the package manager removes the conflicting package. A dpkg
replacement reported as `in-favour` of the other Private AI Proxy package skips
cleanup.

If you already removed the CLI and an agent still points to the Local API,
reinstall the same package or use another `pap` installation to retry
`pap stop --offline --for-uninstall` with the original user data. If you explicitly disabled
login supervision before uninstalling, `pap service enable` turns it back on
after reinstalling.
