# ACI clients

[`@phala/aci-verifier`](verifier-ts/README.md) is the only TypeScript package
in this workspace. Use [`pap`](../apps/desktop/docs/cli.md), the Private AI
Proxy CLI, for coding agents and provider access.
[Client architecture](architecture.md) describes the verification boundaries.

## Choose a client

| Need | Use |
| --- | --- |
| Run a familiar curl request over an attested channel | [`pap curl`](../apps/desktop/docs/cli.md#one-pinned-curl-request) |
| Verify one chat request, receipt, and cited session | [`pap send`](../docs/quickstart.md#5-verify-one-inference-end-to-end) |
| Point a coding agent at a local verified endpoint | [`pap serve`](../docs/quickstart.md#4-use-it-as-a-local-endpoint) |
| Verify artifacts in a browser, or add pinned `fetch` to a Node or Bun SDK | [`@phala/aci-verifier`](verifier-ts/README.md) |

## Retired packages

`@phala/aci-provider`, `@phala/pi-provider-aci`, `pi-provider-redpill`,
`pi-provider-phala-cloud`, `@phala/opencode-provider-aci`,
`opencode-provider-redpill`, and `opencode-provider-phala-cloud` are retired;
use `pap` instead. See the [CLI guide](../apps/desktop/docs/cli.md).

## Maintainer guides

- [Client architecture](architecture.md): component ownership, the shared
  trust contract, response verification, and release acceptance.
- [Coding agents](coding-agents.md): the supported `pap` integration.
- [Releasing](releasing.md): the verifier npm release process.
