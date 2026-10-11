# Pi ACI providers

- [`@phala/pi-provider-aci`](packages/pi-provider-aci) is the vendor-neutral
  Pi adapter.
- [`pi-provider-redpill`](packages/pi-provider-redpill) supplies the RedPill
  endpoint, identity, and environment names.
- [`pi-provider-phala-cloud`](packages/pi-provider-phala-cloud) supplies the
  Phala Cloud endpoint, identity, and account flow.

The branded packages are thin profiles over the neutral package's
`createProvider()`. Several branded providers can run in one Pi process
without sharing provider IDs, environment names, config paths, commands, or
connection state.

The adapter registers Pi's native `Provider` and `ApiKeyAuth` interfaces and
uses Pi's dynamic-catalog helper, so Pi owns catalog refresh, offline
restoration, persistence, credential storage, and default-model selection. The
`openai-completions` adapter receives the verified connection's scoped fetch
through `StreamOptions.fetch`. Each Pi session opens a fresh verified connection
and closes it on shutdown.

[Coding-agent integrations](../coding-agents.md#pi) covers installation, login,
settings, and commands. [Client architecture](../architecture.md) covers what
the shared verifier checks.

## Run from a source checkout

```bash
npm --prefix clients ci
npm --prefix clients run build
export ACI_BASE_URL=https://<your-gateway>/v1
export ACI_API_KEY=...
pi -e clients/pi-provider/packages/pi-provider-aci
```
