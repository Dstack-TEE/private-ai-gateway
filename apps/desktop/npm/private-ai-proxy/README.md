# Private AI Proxy

Private AI Proxy runs AI coding agents through a local, user-owned gateway that verifies confidential-computing evidence before forwarding requests.

## Install

```sh
npm install --global private-ai-proxy
```

The package installs `pap` as the preferred command. `private-ai-proxy` is the
full-name alias and `aci` is a legacy alias kept for existing scripts; all three
run the same CLI. npm selects the native package for the current operating system and CPU
architecture.

Do not install with `--omit=optional`: the platform-specific executable is
delivered as an optional dependency.

## Use

```sh
pap --help
pap service start
pap service status
pap app open --web
```

The CLI, service, and credential helper are distributed together so the local
service can start the matching binaries from the same installation. The
service can also host the management UI on `127.0.0.1` (off by default): enable
it with `pap settings set webUi true`, then sign in with the one-time link from
`pap app open --web`. For another machine, prefer an SSH tunnel or Tailscale;
read [Remote Access](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/apps/desktop/docs/cli.md#remote-access)
before listening on a LAN address over plain HTTP.

Source and release notes: https://github.com/Dstack-TEE/private-ai-gateway
