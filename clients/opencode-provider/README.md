# OpenCode ACI providers

- [`@phala/opencode-provider-aci`](packages/opencode-provider-aci) is the
  vendor-neutral OpenCode adapter.
- [`opencode-provider-redpill`](packages/opencode-provider-redpill) supplies
  the RedPill endpoint, identity, and environment names.
- [`opencode-provider-phala-cloud`](packages/opencode-provider-phala-cloud)
  supplies the Phala Cloud endpoint and identity.

Both branded packages support direct API keys. Only the Phala Cloud package
adds its device login; RedPill does not currently expose account OAuth. Every
package default-exports one definition that supports both OpenCode V1
(`server`) and OpenCode V2 (`id` and `setup`), so OpenCode `1.18.29` or newer
and OpenCode 2 both load it. The provider is created by the plugin, so plugin
installation or initialization failure cannot leave a separately configured
ordinary HTTPS provider behind.

Install through OpenCode's native plugin command:

```sh
opencode plugin opencode-provider-redpill --global
opencode plugin opencode-provider-phala-cloud --global
```

Restart OpenCode after installing. Run `/connect`, select RedPill AI or Phala
Cloud, then run `/models` and select a `redpill/` or `phala/` model. Phala Cloud
offers both its account device flow and manual API-key entry; RedPill currently
offers API-key entry. OpenCode persists the plugin entry and credential in its
own stores. No hand-written provider block or adapter-owned credential file is
required.

Each plugin also registers one provider-scoped, read-only inspection tool:
`aci_inspect`, `redpill_aci_inspect`, or `phala_aci_inspect`. It reports the
verified connection and attestation, lists retained receipts, verifies a
receipt, or verifies a content-addressed session without returning prompts,
responses, or raw evidence. Provider-scoped commands expose those actions as
`/<id>-attestation`, `/<id>-receipts`, `/<id>-receipt [receipt-id]`, and
`/<id>-session <session-id>`. OpenCode commands are prompt templates: they ask
the selected model to invoke that local tool and return its output. Verification
remains automatic and fail closed; the commands are an inspection surface only.
