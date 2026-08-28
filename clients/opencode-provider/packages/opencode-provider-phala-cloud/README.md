# `opencode-provider-phala-cloud`

Phala Cloud's native OpenCode provider. OpenCode `1.18.24` or newer is
recommended. Install it globally through OpenCode's official plugin command:

```sh
opencode plugin opencode-provider-phala-cloud --global
```

Restart OpenCode after installing, then use its native provider and model
pickers:

```text
/connect
# search for Phala Cloud
# choose Phala Cloud account or Phala Cloud API key
/models
# search for phala/ and select a model
```

The account method uses Phala Cloud's device flow to issue a Confidential AI
key. It returns that key through OpenCode's documented browser-authorization
hook, and OpenCode stores it as its native API credential. The plugin does not
implement its own token or credential store. `PHALA_AI_API_KEY` is also
supported for the current process, but environment variables are not copied
into the auth store. Select `phala/<model-id>` in OpenCode; inference travels
only through the attested, TLS-pinned ACI connection.

Attestation is verified before model discovery, and each response receipt is
verified before OpenCode can finish the turn. These commands display the local
evidence; they do not enable or weaken enforcement:

```text
/phala-attestation
/phala-receipts
/phala-receipt [receipt-id]
/phala-session <session-id>
```

They dispatch the read-only `phala_aci_inspect` tool. The local wire-digest
history keeps the latest 32 receipt-bearing requests by default and is cleared
when OpenCode exits. Gateway receipt and session artifacts have their own
server-side retention.

Do not add a separate `provider.phala` block. The plugin registers the provider,
model catalog, verified fetch, and auth loader through OpenCode's native
server-plugin API.
