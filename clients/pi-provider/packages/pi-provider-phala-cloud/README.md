# pi-provider-phala-cloud

Phala Cloud Confidential AI for Pi, powered by [private-ai-gateway].

A thin, Phala Cloud-branded distribution of the vendor-neutral
[`@phala/pi-provider-aci`](https://www.npmjs.com/package/@phala/pi-provider-aci): standard
chat plus **attested TLS (SPKI) pinning** — the prompt and reply are readable
only by the attested workload. Every response receipt is verified before Pi
can finish the model turn or continue its tool loop.

## Install

```bash
pi install npm:pi-provider-phala-cloud
```

Start Pi, complete the native device login, wait for the verified connection,
then save a default model from the native model picker:

```text
/login phala
# approve the device login and wait for the footer to show aci-verified
/model
# search for phala/, select a model, and press Ctrl+S
```

The device flow issues a Confidential AI API key and returns it to Pi's official
API-key auth interface. Pi stores the credential in `~/.pi/agent/auth.json`, the
refreshed catalog in `~/.pi/agent/models-store.json`, and the saved default in
`~/.pi/agent/settings.json`. These values survive a restart and the cached
catalog remains available offline. As an alternative for one process, set
`PHALA_AI_API_KEY`; Pi does not copy environment variables into its credential
store.

Config: `/phala-settings` · Attestation status: `/phala-attestation` · Receipt
history and audit: `/phala-receipt` · Session inspection: `/phala-session`

Interchangeable with `pi-provider-redpill` — both share the same
protocol core and pin the same attested workload. If you operate your own private-ai-gateway, use the neutral
[`@phala/pi-provider-aci`](https://www.npmjs.com/package/@phala/pi-provider-aci) instead.

[private-ai-gateway]: https://github.com/Dstack-TEE/private-ai-gateway
