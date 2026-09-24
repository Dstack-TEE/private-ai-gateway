# Private AI Proxy

Private AI Proxy runs AI coding agents through a local, user-owned gateway that verifies confidential-computing evidence before forwarding requests.

## Install

```sh
npm install --global private-ai-proxy
```

The package installs `pap` as the preferred command. `private-ai-proxy` is the
full-name alias and `aci` is a legacy alias kept for existing scripts; all three
run the same CLI. The native executables for each supported platform ship in an
optional dependency such as `@phala/private-ai-proxy-linux-x64`, and your package
manager installs only the one for the current operating system and CPU
architecture. Do not install with `--omit=optional` or `--no-optional`.

Supported platforms are macOS, Linux with glibc 2.35 or newer, and Windows, each
on arm64 and x64. musl-based Linux distributions such as Alpine are not
supported.

## Use

```sh
pap --help
pap service start
pap service status
pap app open --web
```

The CLI, service, and credential helper are distributed together so the local
service can start the matching binaries from the same installation. The
service can also host the management UI on `127.0.0.1` (off by default): set a
password with `pap settings set webUiPassword`, enable it with
`pap settings set webUi true`, then open the address from `pap app open --web`
and sign in. For another machine, prefer an SSH tunnel or Tailscale;
read [Remote Access](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/apps/desktop/docs/cli.md#remote-access)
before listening on a LAN address over plain HTTP.

Source and release notes: https://github.com/Dstack-TEE/private-ai-gateway
