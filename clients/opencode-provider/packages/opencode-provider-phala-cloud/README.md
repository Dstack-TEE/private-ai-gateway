# `opencode-provider-phala-cloud`

Phala Cloud's native OpenCode provider. Install it through OpenCode's official
plugin command:

```sh
opencode plugin opencode-provider-phala-cloud
```

Pass `--global` to install it in the global OpenCode config. Start the official
provider login and choose the Phala Cloud account method or API-key method:

```sh
opencode providers login --provider phala
```

The account method uses Phala Cloud's device flow to issue a Confidential AI
key. It returns that key through OpenCode's documented browser-authorization
hook, and OpenCode stores it as its native API credential. The plugin does not
implement its own token or credential store. `PHALA_AI_API_KEY` is also
supported for the current process, but environment variables are not copied
into the auth store. Select `phala/<model-id>` in OpenCode; inference travels
only through the attested, TLS-pinned ACI connection.

Use the read-only `phala_aci_inspect` tool to inspect the verified attestation,
retained receipt history, a receipt audit, or a content-addressed session audit.

Do not add a separate `provider.phala` block. The plugin registers the provider,
model catalog, verified fetch, and auth loader through OpenCode's native
server-plugin API.
