# Private AI Proxy

Private AI Proxy runs AI coding agents through a local, user-owned gateway that verifies confidential-computing evidence before forwarding requests.

## Install

```sh
npm install --global private-ai-proxy
```

The package installs `private-ai-proxy` and the shorter aliases `pap` and `aci`. npm selects the native package for the current operating system and CPU architecture.

Do not install with `--omit=optional`: the platform-specific executable is delivered as an optional dependency.

## Use

```sh
private-ai-proxy --help
private-ai-proxy service start
private-ai-proxy service status
```

The CLI, service, and credential helper are distributed together so the local service can start the matching binaries from the same installation.

Source and release notes: https://github.com/Dstack-TEE/private-ai-gateway
