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

The plugin discovers the public `/v1/models` catalog over the verified
connection without sending the inference API key. OpenCode stores the key and
attaches it to model requests through its native auth loader.

The read-only `aci_inspect` tool exposes five actions: `status`, `attestation`,
`receipts`, `receipt`, and `session`. Receipt inspection verifies the latest
recorded exchange when no id is supplied. Session inspection requires the bare
64-hex session id and verifies its content address, API version, validity
window, and evidence digest. Attestation and receipt views include the same
key, signature, routing, and wire-hash metadata exposed by the Pi audit
commands. The tool returns verification metadata only, never model traffic or
raw evidence. Branded plugins scope the tool name as `<provider>_aci_inspect`.

Do not also configure a separate `provider.aci`. The plugin owns that provider
so installation, attestation, or channel-binding failure leaves no ordinary
HTTPS path available.
