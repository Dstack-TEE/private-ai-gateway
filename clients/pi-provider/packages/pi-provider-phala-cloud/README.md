# pi-provider-phala-cloud

Phala Cloud Confidential AI for Pi, powered by [private-ai-gateway].

A Phala Cloud-branded distribution of the vendor-neutral
[`@phala/pi-provider-aci`](https://www.npmjs.com/package/@phala/pi-provider-aci).
It verifies the Phala Cloud gateway and pins its attested TLS key before
sending model traffic, and it verifies every response receipt before Pi
finishes the model turn.

## Install

```bash
pi install npm:pi-provider-phala-cloud
```

Start Pi, complete the native login, wait for the verified connection, then
save a default model from the native model picker:

```text
/login phala
# choose Phala Cloud account or Phala Cloud API key
# complete login and wait for the footer to show aci-verified
/model
# search for phala/, select a model, and press Ctrl+S
```

The account flow issues a Confidential AI API key; the manual method accepts an
existing key. Both give Pi the same native API-key credential.
`PHALA_AI_API_KEY` also works for the current process, but Pi does not save it.

## Configure

The default gateway is `https://inference.phala.com/v1`; override it with
`PHALA_BASE_URL`.

Config: `/phala-settings` · Attestation status: `/phala-attestation` · Receipt
history: `/phala-receipts` · Receipt audit: `/phala-receipt` · Session
inspection: `/phala-session`

## Learn more

- [Coding-agent integrations](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/clients/coding-agents.md#pi):
  persistence, settings, and local receipt history.
- [Client architecture](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/clients/architecture.md):
  what the verifier checks and how to pin reviewed releases.

If you operate your own private-ai-gateway, use the neutral
[`@phala/pi-provider-aci`](https://www.npmjs.com/package/@phala/pi-provider-aci)
instead.

[private-ai-gateway]: https://github.com/Dstack-TEE/private-ai-gateway
