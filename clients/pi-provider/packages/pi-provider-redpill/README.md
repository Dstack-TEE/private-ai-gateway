# pi-provider-redpill

RedPill AI for Pi, powered by [private-ai-gateway].

A RedPill-branded distribution of the vendor-neutral
[`@phala/pi-provider-aci`](https://www.npmjs.com/package/@phala/pi-provider-aci).
It verifies the RedPill gateway and pins its attested TLS key before sending
model traffic, and it verifies every response receipt before Pi finishes the
model turn.

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

`REDPILL_AI_API_KEY` also works for the current process, but Pi does not save
it.

## Configure

The default gateway is `https://tee.redpill.ai/v1`; override it with
`REDPILL_BASE_URL`. `tee.redpill.ai` and `inference.phala.com` enforce TEE-only
routing and accept the same key. `api.redpill.ai` is the general API endpoint,
not the default verified transport.

Config: `/redpill-settings` · Attestation status: `/redpill-attestation` ·
Receipt history: `/redpill-receipts` · Receipt audit: `/redpill-receipt` ·
Session inspection: `/redpill-session`

## Learn more

- [Coding-agent integrations](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/clients/coding-agents.md#pi):
  persistence, settings, and local receipt history.
- [Client architecture](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/clients/architecture.md):
  what the verifier checks and how to pin reviewed releases.

If you operate your own private-ai-gateway, use the neutral
[`@phala/pi-provider-aci`](https://www.npmjs.com/package/@phala/pi-provider-aci)
instead.

[private-ai-gateway]: https://github.com/Dstack-TEE/private-ai-gateway
