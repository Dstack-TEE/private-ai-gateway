# Coding agents over ACI

Use [`pap`](../apps/desktop/docs/cli.md), the Private AI Proxy CLI, for coding
agents and provider access. `pap serve` exposes a local OpenAI-compatible
endpoint backed by an attested connection. Follow the
[CLI guide](../apps/desktop/docs/cli.md) for setup and usage.

The TypeScript workspace contains only
[`@phala/aci-verifier`](verifier-ts/README.md), for browser artifact verification
and pinned Node and Bun SDK transports. The former provider integrations are
listed under [Retired packages](README.md#retired-packages).
