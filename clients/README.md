# ACI clients

Choose the smallest client that matches your host. Every client verifies the
remote workload and its channel before sending model request bytes;
[Client architecture](architecture.md) defines what each one checks.

## Choose a client

| Need | Use |
| --- | --- |
| Run a familiar curl request over an attested channel | [`pap curl`](../apps/desktop/docs/cli.md#one-pinned-curl-request) |
| Verify one chat request, receipt, and cited session | [`pap send`](../docs/quickstart.md#5-verify-one-inference-end-to-end) |
| Point a base-URL-only tool at a local verified endpoint | [`pap serve`](../docs/quickstart.md#4-use-it-as-a-local-endpoint) |
| Verify artifacts in a browser, or add pinned `fetch` to a Node or Bun SDK | [`@phala/aci-verifier`](verifier-ts/README.md) |
| Build a provider adapter for another host | [`@phala/aci-provider`](provider/README.md) |
| Use ACI in Pi or OpenCode | [Coding-agent integrations](coding-agents.md) |

## Packages

Besides `@phala/aci-verifier`, the client workspace publishes these packages:

| Host | Neutral package | RedPill | Phala Cloud |
| --- | --- | --- | --- |
| Shared kernel | [`@phala/aci-provider`](provider/README.md) | Shared profile | Shared profile and account flow |
| Pi | [`@phala/pi-provider-aci`](pi-provider/packages/pi-provider-aci/README.md) | [`pi-provider-redpill`](pi-provider/packages/pi-provider-redpill/README.md) | [`pi-provider-phala-cloud`](pi-provider/packages/pi-provider-phala-cloud/README.md) |
| OpenCode | [`@phala/opencode-provider-aci`](opencode-provider/packages/opencode-provider-aci/README.md) | [`opencode-provider-redpill`](opencode-provider/packages/opencode-provider-redpill/README.md) | [`opencode-provider-phala-cloud`](opencode-provider/packages/opencode-provider-phala-cloud/README.md) |

## Maintainer guides

- [Client architecture](architecture.md): component ownership, the shared
  trust contract, response verification, and release acceptance.
- [Coding-agent integrations](coding-agents.md): installation and host
  behavior for Pi and OpenCode.
- [Releasing](releasing.md): the coordinated npm release process.
