# pi-provider-redpill

Attested AI for Pi, powered by [private-ai-gateway].

A thin, Redpill-branded distribution of the vendor-neutral
[`@phala/pi-provider-aci`](https://www.npmjs.com/package/@phala/pi-provider-aci): standard
chat plus **attested TLS (SPKI) pinning** — the prompt and reply are readable
only by the attested workload. Every response receipt is verified before Pi
can finish the model turn or continue its tool loop.

## Install

```bash
pi install npm:pi-provider-redpill
export REDPILL_LLM_API_KEY=...
```

The default ACI gateway is `https://tee.redpill.ai/v1`; override with
`REDPILL_BASE_URL`. `tee.redpill.ai` and `inference.phala.com` enforce TEE-only
routing and accept the same key. `api.redpill.ai` is the general API endpoint,
not the default verified transport.

```text
/model redpill/deepseek/deepseek-v4-flash
```

Config: `/redpill-settings` · Attestation status: `/attestation` · Receipt
history and audit: `/aci-receipt`

Interchangeable with `pi-provider-phala-cloud` — both share the same
protocol core and pin the same attested workload. If you operate your own private-ai-gateway, use the neutral
[`@phala/pi-provider-aci`](https://www.npmjs.com/package/@phala/pi-provider-aci) instead.

[private-ai-gateway]: https://github.com/Dstack-TEE/private-ai-gateway
