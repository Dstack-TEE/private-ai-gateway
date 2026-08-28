# pi-provider-redpill

Attested AI for Pi, powered by [private-ai-gateway].

A thin, RedPill-branded distribution of the vendor-neutral
[`@phala/pi-provider-aci`](https://www.npmjs.com/package/@phala/pi-provider-aci): standard
chat plus **attested TLS (SPKI) pinning** — the prompt and reply are readable
only by the attested workload. Every response receipt is verified before Pi
can finish the model turn or continue its tool loop.

## Install

```bash
pi install npm:pi-provider-redpill
pi
```

In Pi, store the API key through the native login flow, wait for the verified
connection, then save a default model from the native model picker:

```text
/login redpill
# paste the RedPill API key and wait for the footer to show aci-verified
/model
# search for redpill/, select a model, and press Ctrl+S
```

Pi stores the credential in `~/.pi/agent/auth.json`, the refreshed catalog in
`~/.pi/agent/models-store.json`, and the saved default in
`~/.pi/agent/settings.json`. These values survive a restart and the cached
catalog remains available offline. As an alternative for one process, set
`REDPILL_AI_API_KEY`; Pi does not copy environment variables into its credential
store.

The default ACI gateway is `https://tee.redpill.ai/v1`; override with
`REDPILL_BASE_URL`. `tee.redpill.ai` and `inference.phala.com` enforce TEE-only
routing and accept the same key. `api.redpill.ai` is the general API endpoint,
not the default verified transport.

Config: `/redpill-settings` · Attestation status: `/redpill-attestation` · Receipt
history: `/redpill-receipts` · Receipt audit: `/redpill-receipt` · Session
inspection: `/redpill-session`

Attestation and every response receipt are verified automatically. The commands
only display the evidence or rerun an audit. The local wire-digest history keeps
the latest 32 receipt-bearing requests by default and is cleared when Pi exits;
gateway receipt and session artifacts have their own server-side retention.

Interchangeable with `pi-provider-phala-cloud` — both share the same
protocol core and pin the same attested workload. If you operate your own private-ai-gateway, use the neutral
[`@phala/pi-provider-aci`](https://www.npmjs.com/package/@phala/pi-provider-aci) instead.

[private-ai-gateway]: https://github.com/Dstack-TEE/private-ai-gateway
