# OpenCode ACI providers

- [`@phala/opencode-provider-aci`](packages/opencode-provider-aci) is the
  vendor-neutral OpenCode adapter.
- [`opencode-provider-redpill`](packages/opencode-provider-redpill) supplies
  the RedPill endpoint, identity, environment names, and bootstrap catalog.
- [`opencode-provider-phala-cloud`](packages/opencode-provider-phala-cloud)
  supplies the Phala Cloud endpoint and identity.

The branded packages support Phala Cloud device login and direct API keys.
All packages use OpenCode's v1 server-plugin manifest and run on Bun. The
provider is created by the plugin, so plugin installation or initialization
failure cannot leave a separately configured ordinary HTTPS provider behind.
