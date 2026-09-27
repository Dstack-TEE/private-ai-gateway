# Install Private AI Proxy

Private AI Proxy ships as the `pap` CLI and a desktop app. Every installation
method below installs the CLI; Homebrew and GitHub Releases also offer the
desktop app. [The CLI reference](../apps/desktop/docs/cli.md) describes `pap`
and its aliases.

The CLI runs on macOS, Windows, and Linux, on arm64 and x64. Linux needs glibc
2.35 or newer; musl distributions such as Alpine are not supported.

The steps below install the stable release. `pap curl` and the key-custody
flags (`--accept-subject` with `--accept-dstack-kms-root-public-key`) need
0.2.0-beta.6 or later. Install that with
`npm install --global private-ai-proxy@beta` or the install script's beta
channel (`--channel beta`, or `-Channel beta` on Windows).

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

## Homebrew

Install the CLI Formula, or the Cask with the desktop app for macOS, directly
from the official tap:

```sh
brew install dstack-tee/private-ai/private-ai-proxy
brew install --cask dstack-tee/private-ai/private-ai-proxy
```

To use the shorter commands after tapping once:

```sh
brew tap dstack-tee/private-ai
brew install private-ai-proxy
brew install --cask private-ai-proxy
```

The Formula exposes `pap`, `private-ai-proxy`, and `aci`. Homebrew owns upgrades
and removal for both the Formula and Cask installations.

The tap carries stable releases only. It is updated by pull requests that the
tap's own workflow opens after a stable release and a maintainer merges, so it
can trail the latest GitHub release. Use npm or the install script for beta
versions or when you need a release the tap does not have yet.

## macOS and Linux install script

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
three commands. It never edits shell startup files.

Options:

```text
--channel stable|beta
--version VERSION
--install-dir PATH
--bin-dir PATH
```

The defaults are `${XDG_DATA_HOME:-$HOME/.local/share}/private-ai-proxy` for
versioned files and `$HOME/.local/bin` for commands.

## Windows install script

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

## Desktop app and native packages

Signed desktop app installers, portable CLI archives, and Linux DEB, RPM, and
Arch packages are published on the
[GitHub Releases](https://github.com/Dstack-TEE/private-ai-gateway/releases)
page. Native package managers own upgrades and removal for their installations.
