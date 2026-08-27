# OpenCode ACI providers

- [`@phala/opencode-provider-aci`](packages/opencode-provider-aci) is the
  vendor-neutral OpenCode adapter.
- [`opencode-provider-redpill`](packages/opencode-provider-redpill) supplies
  the RedPill endpoint, identity, and environment names.
- [`opencode-provider-phala-cloud`](packages/opencode-provider-phala-cloud)
  supplies the Phala Cloud endpoint and identity.

Both branded packages support direct API keys. Only the Phala Cloud package
adds its device login; Redpill does not currently expose account OAuth. All
packages use OpenCode's v1 server-plugin manifest and run on Bun. The provider
is created by the plugin, so plugin installation or initialization failure
cannot leave a separately configured ordinary HTTPS provider behind.

Each plugin also registers one provider-scoped, read-only inspection tool:
`aci_inspect`, `redpill_aci_inspect`, or `phala_aci_inspect`. It reports the
verified connection and attestation, lists retained receipts, verifies a
receipt, or verifies a content-addressed session without returning prompts,
responses, or raw evidence.
