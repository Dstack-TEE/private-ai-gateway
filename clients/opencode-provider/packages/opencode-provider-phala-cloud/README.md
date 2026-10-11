# `opencode-provider-phala-cloud`

Phala Cloud's native OpenCode provider. It verifies the Phala Cloud gateway and
pins its attested TLS key before sending model traffic, and it verifies each
response receipt before OpenCode finishes the turn.

## Install

OpenCode `1.18.24` or newer is recommended. Install the plugin globally
through OpenCode's official plugin command:

```sh
opencode plugin opencode-provider-phala-cloud --global
```

Restart OpenCode, then use its native provider and model pickers:

```text
/connect
# search for Phala Cloud
# choose Phala Cloud account or Phala Cloud API key
/models
# search for phala/ and select a model
```

The account method uses Phala Cloud's device flow to issue a Confidential AI
key, and OpenCode stores it as its native API credential. `PHALA_AI_API_KEY`
also works for the current process, but OpenCode does not save it.

Do not add a separate `provider.phala` block. The plugin registers the
provider, model catalog, verified fetch, and auth loader.

## Inspect

These commands display local evidence; they do not enable or weaken
enforcement:

```text
/phala-attestation
/phala-receipts
/phala-receipt [receipt-id]
/phala-session <session-id>
```

## Learn more

- [Coding-agent integrations](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/clients/coding-agents.md#opencode):
  settings and local receipt history.
- [Client architecture](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/clients/architecture.md):
  what the verifier checks and how to pin reviewed releases.
