# `opencode-provider-redpill`

RedPill's native OpenCode provider. It verifies the RedPill gateway and pins
its attested TLS key before sending model traffic, and it verifies each
response receipt before OpenCode finishes the turn.

## Install

OpenCode `1.18.24` or newer is recommended. Install the plugin globally
through OpenCode's official plugin command:

```sh
opencode plugin opencode-provider-redpill --global
```

Restart OpenCode, then use its native provider and model pickers:

```text
/connect
# search for RedPill AI and enter the API key
/models
# search for redpill/ and select a model
```

OpenCode saves the plugin entry and credential in its own stores.
`REDPILL_AI_API_KEY` also works for the current process, but OpenCode does not
save it. RedPill does not currently offer account login.

Do not add a separate `provider.redpill` block. The plugin registers the
provider, model catalog, verified fetch, and auth loader.

## Inspect

These commands display local evidence; they do not enable or weaken
enforcement:

```text
/redpill-attestation
/redpill-receipts
/redpill-receipt [receipt-id]
/redpill-session <session-id>
```

## Learn more

- [Coding-agent integrations](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/clients/coding-agents.md#opencode):
  settings and local receipt history.
- [Client architecture](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/clients/architecture.md):
  what the verifier checks and how to pin reviewed releases.
