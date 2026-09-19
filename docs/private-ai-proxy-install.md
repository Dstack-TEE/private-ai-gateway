# Install Private AI Proxy

`pap` is the preferred command. `private-ai-proxy` is the full-name alias and
`aci` is the protocol-focused alias; all three run the same native CLI.

## npm

```sh
npm install --global private-ai-proxy
pap --help
```

npm selects the native package for the current operating system and CPU
architecture. Do not use `--omit=optional` because the native payload is an
optional dependency.

## macOS and Linux install script

Review and run the official installer:

```sh
curl -fsSLO https://raw.githubusercontent.com/Dstack-TEE/private-ai-gateway/main/scripts/install-private-ai-proxy.sh
sh install-private-ai-proxy.sh
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

Download the script so it can be reviewed before execution:

```powershell
Invoke-WebRequest `
  https://raw.githubusercontent.com/Dstack-TEE/private-ai-gateway/main/scripts/install-private-ai-proxy.ps1 `
  -OutFile install-private-ai-proxy.ps1
powershell -ExecutionPolicy Bypass -File .\install-private-ai-proxy.ps1
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

## Native packages and desktop application

Signed desktop installers, portable CLI archives, and Linux DEB, RPM, and Arch
packages are published on the
[GitHub Releases](https://github.com/Dstack-TEE/private-ai-gateway/releases)
page. Native package managers own upgrades and removal for their installations.
