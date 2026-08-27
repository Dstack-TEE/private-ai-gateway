# `@phala/opencode-provider-aci`

Native OpenCode provider for an ACI gateway. The plugin registers the provider,
discovers its live model catalog over an attested and SPKI-pinned connection,
maps reasoning/tool/modality/cost/limit metadata, and verifies every inference
receipt before the response stream can finish.

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "plugin": [
    [
      "@phala/opencode-provider-aci",
      {
        "baseURL": "https://gateway.example.com/v1",
        "trust": {
          "acceptedComposeHashes": ["<reviewed-compose-sha256>"],
        },
      },
    ],
  ],
}
```

Set `ACI_API_KEY` or run `opencode providers login`. Select a discovered model
as `aci/<model-id>`.

Do not also configure a separate `provider.aci`. The plugin owns that provider
so installation, attestation, or channel-binding failure leaves no ordinary
HTTPS path available.
