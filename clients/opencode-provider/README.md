# OpenCode ACI providers

- [`@phala/opencode-provider-aci`](packages/opencode-provider-aci) is the
  vendor-neutral OpenCode adapter.
- [`opencode-provider-redpill`](packages/opencode-provider-redpill) supplies
  the RedPill endpoint, identity, and environment names.
- [`opencode-provider-phala-cloud`](packages/opencode-provider-phala-cloud)
  supplies the Phala Cloud endpoint, identity, and device login.

All packages use OpenCode's v1 server-plugin manifest and run on Bun. The
plugin creates the provider itself, so a plugin installation or initialization
failure cannot leave a separately configured ordinary HTTPS provider behind.

Each plugin registers one provider-scoped, read-only inspection tool:
`aci_inspect`, `redpill_aci_inspect`, or `phala_aci_inspect`. Its commands are
OpenCode prompt templates that ask the selected model to call that local tool
and return its output.

[Coding-agent integrations](../coding-agents.md#opencode) covers installation,
login, settings, and commands. [Client architecture](../architecture.md) covers
what the shared verifier checks.
