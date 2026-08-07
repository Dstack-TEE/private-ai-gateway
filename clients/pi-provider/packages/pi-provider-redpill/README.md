# pi-provider-redpill

Attested AI for Pi, powered by [private-ai-gateway].

A thin, Redpill-branded distribution of the vendor-neutral
[`@phala/pi-provider-aci`](https://www.npmjs.com/package/@phala/pi-provider-aci): standard
chat plus **attested TLS (SPKI) pinning** and **per-response receipt
verification** in the footer (`verified` / `routed` / `mismatch`).

## Install

```bash
pi install npm:pi-provider-redpill
export REDPILL_LLM_API_KEY=...
```

The default gateway is `https://api.redpill.ai/v1`; override with `REDPILL_BASE_URL`.
`api.redpill.ai`, `tee.redpill.ai`, and `inference.phala.com` are the same backend
and accept the same key.

```text
/model redpill/deepseek/deepseek-v4-flash
```

Config: `/redpill-settings` · Attestation status: `/attestation`

Interchangeable with `pi-provider-phala-cloud` — both share the same verified
protocol core. If you operate your own private-ai-gateway, use the neutral
[`@phala/pi-provider-aci`](https://www.npmjs.com/package/@phala/pi-provider-aci) instead.

[private-ai-gateway]: https://github.com/Dstack-TEE/private-ai-gateway