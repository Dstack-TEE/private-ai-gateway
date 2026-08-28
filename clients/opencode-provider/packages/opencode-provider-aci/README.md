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

After adding the configured plugin tuple above, restart OpenCode and use its
native provider and model pickers:

```text
/connect
# search for ACI and enter the API key
/models
# search for aci/ and select a model
```

The neutral package has no default gateway, so `baseURL` must be configured.
`ACI_API_KEY` is also supported for the current process, but environment
variables are not copied into OpenCode's auth store.

The plugin discovers the public `/v1/models` catalog over the verified
connection without sending the inference API key. OpenCode stores the key and
attaches it to model requests through its native auth loader. The plugin uses
OpenCode's server-plugin, provider config, auth, model, and disposal hooks; it
does not maintain parallel config or credential files.

The plugin registers `/aci-attestation`, `/aci-receipts`, `/aci-receipt [id]`,
and `/aci-session <id>` as native OpenCode custom commands. They dispatch the
read-only `aci_inspect` tool, whose five actions are `status`, `attestation`,
`receipts`, `receipt`, and `session`. Receipt inspection verifies the latest
recorded exchange when no id is supplied. Session inspection requires the bare
64-hex session id and verifies its content address, API version, validity
window, and evidence digest. The tool returns verification metadata only,
never model traffic or raw evidence. Branded plugins scope both commands and
the tool name to their provider id.

Attestation and response receipt verification are automatic and fail closed;
the commands only display evidence or rerun an audit. The local wire-digest
history keeps the latest 32 receipt-bearing requests by default and is cleared
when OpenCode exits. Credential persistence and gateway artifact retention are
independent of this local history.

Do not also configure a separate `provider.aci`. The plugin owns that provider
so installation, attestation, or channel-binding failure leaves no ordinary
HTTPS path available.
