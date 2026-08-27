# `opencode-provider-redpill`

RedPill's native OpenCode provider. Install it in your OpenCode configuration:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "plugin": ["opencode-provider-redpill"],
}
```

Run `opencode providers login` and choose Redpill AI to sign in with a Phala
Cloud account, or set `REDPILL_LLM_API_KEY`. Then select `redpill/<model-id>`.
The plugin discovers current TEE models and sends inference only through an
attested, TLS-pinned connection. Every response receipt is verified before
OpenCode can finish the model turn or continue its tool loop.
