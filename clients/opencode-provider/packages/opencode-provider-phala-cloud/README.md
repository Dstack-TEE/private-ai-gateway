# `opencode-provider-phala-cloud`

Phala Cloud's native OpenCode provider. Install it for the current project:

```sh
opencode plugin opencode-provider-phala-cloud
```

Pass `--global` to install it in the global OpenCode config. Then run
`opencode providers login` and choose Phala Cloud, or set
`PHALA_AI_API_KEY`. Then select `phala/<model-id>`. Account login uses the
Phala Cloud device flow to issue a Confidential AI key; inference still travels
only through the attested, TLS-pinned ACI connection. OpenCode calls browser
authorization methods `oauth`, but this flow returns and stores an API-key
credential rather than fabricating refreshable OAuth tokens.

Use the read-only `phala_aci_inspect` tool to inspect the verified attestation,
retained receipt history, a receipt audit, or a content-addressed session audit.
