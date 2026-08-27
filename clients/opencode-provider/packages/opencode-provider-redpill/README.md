# `opencode-provider-redpill`

RedPill's native OpenCode provider. Install it in your OpenCode configuration:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "plugin": ["opencode-provider-redpill"],
}
```

Run `opencode providers login` and paste a Redpill API key, or set
`REDPILL_LLM_API_KEY`. Redpill does not currently expose account OAuth. Then
select `redpill/<model-id>`. The plugin discovers current TEE models and sends
inference only through an attested, TLS-pinned connection. Every response
receipt is verified before OpenCode can finish the model turn or continue its
tool loop.

Use the read-only `redpill_aci_inspect` tool to inspect the verified
attestation, retained receipt history, a receipt audit, or a content-addressed
session audit.
