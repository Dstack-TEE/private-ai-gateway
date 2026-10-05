# `@phala/opencode-provider-aci`

Native OpenCode provider for an Attested Confidential Inference (ACI) gateway.
The plugin verifies the gateway workload and pins its attested TLS key before
sending model traffic, discovers the gateway's models, and verifies every
inference receipt before the response stream can finish.

## Install and configure

The neutral package has no default gateway, so set `baseURL`:

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

`trust.acceptedComposeHashes` lists reviewed compose hashes and is optional.
Restart OpenCode, then use its native provider and model pickers:

```text
/connect
# search for ACI and enter the API key
/models
# search for aci/ and select a model
```

`ACI_API_KEY` also works for the current process, but OpenCode does not save
it. Do not also configure a separate `provider.aci`: the plugin owns that
provider, so a failed attestation or channel check leaves no ordinary HTTPS
path available.

## Inspect

`/aci-attestation`, `/aci-receipts`, `/aci-receipt [id]`, and
`/aci-session <id>` dispatch the read-only `aci_inspect` tool. Its actions are
`status`, `attestation`, `receipts`, `receipt`, and `session`. It returns
verification metadata only, never model traffic or raw evidence. Verification
runs automatically; the commands only display evidence or rerun an audit.

## Build a branded plugin

A programmatic branded plugin may pass a shared `AccountApiKeyAuth` as
`accountAuth`. The plugin maps it into OpenCode's browser auth hook and keeps
the manual API-key method.

## Learn more

- [Coding-agent integrations](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/clients/coding-agents.md#opencode):
  settings, local receipt history, and branded packages.
- [Client architecture](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/clients/architecture.md):
  what the verifier checks, how the model catalog maps, and release
  acceptance.
