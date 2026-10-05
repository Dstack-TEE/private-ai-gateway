# Private AI Proxy

Private AI Proxy runs AI coding agents through a local, user-owned gateway that
verifies confidential-computing evidence before forwarding requests. This
package installs its `pap` CLI together with the per-user backend service and
the agent credential helper.

## Install

```sh
npm install --global private-ai-proxy
```

It runs on macOS, Windows, and Linux with glibc 2.35 or newer (not musl), each
on arm64 and x64. Do not install with `--omit=optional`, and install a tag or
an exact version rather than a prerelease range. The
[install guide](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/docs/private-ai-proxy-install.md#npm)
explains both and lists other ways to install, including the desktop app.

## Use

```sh
pap --help
pap service start
pap service status
```

`pap` is the preferred command. `private-ai-proxy` and the legacy `aci` run the
same CLI.

The service can also serve its management UI to a browser on `127.0.0.1`. It is
off by default:

```sh
pap settings set web-ui.enabled true
pap web-ui password show    # the generated sign-in password
pap app open --web
```

[Web UI](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/apps/desktop/docs/cli.md#web-ui)
covers the password, access from another machine, and the security model.
[The CLI reference](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/apps/desktop/docs/cli.md)
documents every command.

Source and release notes: https://github.com/Dstack-TEE/private-ai-gateway
