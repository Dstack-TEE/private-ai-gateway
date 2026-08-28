# `opencode-provider-redpill`

RedPill's native OpenCode provider. OpenCode `1.18.24` or newer is recommended.
Install it globally through OpenCode's official plugin command:

```sh
opencode plugin opencode-provider-redpill --global
```

Restart OpenCode after installing, then use its native provider and model
pickers:

```text
/connect
# search for RedPill AI and enter the API key
/models
# search for redpill/ and select a model
```

OpenCode persists the plugin entry and credential in its own configuration and
auth store. `REDPILL_AI_API_KEY` is also supported for the current process, but
environment variables are not copied into the auth store. RedPill does not
currently expose account OAuth. Select `redpill/<model-id>` in OpenCode. The
plugin discovers current TEE models and sends
inference only through an attested, TLS-pinned connection. Every response
receipt is verified before OpenCode can finish the model turn or continue its
tool loop.

Attestation is verified before model discovery, and each response receipt is
verified before OpenCode can finish the turn. These commands display the local
evidence; they do not enable or weaken enforcement:

```text
/redpill-attestation
/redpill-receipts
/redpill-receipt [receipt-id]
/redpill-session <session-id>
```

They dispatch the read-only `redpill_aci_inspect` tool. The local wire-digest
history keeps the latest 32 receipt-bearing requests by default and is cleared
when OpenCode exits. Gateway receipt and session artifacts have their own
server-side retention.

Do not add a separate `provider.redpill` block. The plugin registers the
provider, model catalog, verified fetch, and auth loader through OpenCode's
native server-plugin API.
