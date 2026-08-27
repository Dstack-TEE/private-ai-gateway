# `opencode-provider-phala-cloud`

Phala Cloud's native OpenCode provider. Install it in `opencode.json`:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "plugin": ["opencode-provider-phala-cloud"],
}
```

Run `opencode providers login` and choose Phala Cloud, or set
`PHALA_LLM_API_KEY`. Then select `phala/<model-id>`. Account login uses the
Phala Cloud device flow to issue a Confidential AI key; inference still travels
only through the attested, TLS-pinned ACI connection.
