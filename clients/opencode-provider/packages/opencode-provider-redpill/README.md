# `opencode-provider-redpill`

RedPill's native OpenCode provider. Install it through OpenCode's official
plugin command:

```sh
opencode plugin opencode-provider-redpill
```

Pass `--global` to install it in the global OpenCode config. Store a key through
OpenCode's official provider login:

```sh
opencode providers login --provider redpill
```

OpenCode persists the plugin entry and credential in its own configuration and
auth store. `REDPILL_AI_API_KEY` is also supported for the current process, but
environment variables are not copied into the auth store. Redpill does not
currently expose account OAuth. Select `redpill/<model-id>` in OpenCode. The
plugin discovers current TEE models and sends
inference only through an attested, TLS-pinned connection. Every response
receipt is verified before OpenCode can finish the model turn or continue its
tool loop.

Use the read-only `redpill_aci_inspect` tool to inspect the verified
attestation, retained receipt history, a receipt audit, or a content-addressed
session audit.

Do not add a separate `provider.redpill` block. The plugin registers the
provider, model catalog, verified fetch, and auth loader through OpenCode's
native server-plugin API.
