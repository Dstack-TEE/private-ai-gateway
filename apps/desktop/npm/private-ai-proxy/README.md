# Private AI Proxy

Private AI Proxy runs AI coding agents through a local, user-owned gateway that verifies confidential-computing evidence before forwarding requests.

## Install

```sh
npm install --global private-ai-proxy
```

The package installs `pap` as the preferred command. `private-ai-proxy` is the
full-name alias and `aci` is the protocol-focused alias; all three run the same
CLI. npm selects the native package for the current operating system and CPU
architecture.

Do not install with `--omit=optional`: the platform-specific executable is
delivered as an optional dependency.

## Use

```sh
pap --help
pap service start
pap service status
pap ui
```

The CLI, service, and credential helper are distributed together so the local
service can start the matching binaries from the same installation. `pap ui`
opens the embedded management UI on an authenticated loopback-only address.

Source and release notes: https://github.com/Dstack-TEE/private-ai-gateway
