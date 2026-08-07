# pi-provider-phala-cloud

Phala Cloud Confidential AI for Pi, powered by [private-ai-gateway].

A thin, Phala Cloud-branded distribution of the vendor-neutral
[`@phala/pi-provider-aci`](https://www.npmjs.com/package/@phala/pi-provider-aci): standard
chat plus **attested TLS (SPKI) pinning** and **per-response receipt
verification** in the footer (`verified` / `routed` / `mismatch`).

## Install

```bash
pi install npm:pi-provider-phala-cloud
```

Sign in with a Phala Cloud account (`/login phala`, RFC 8628 device flow —
no API key to manage), or set one directly:

```bash
export PHALA_LLM_API_KEY=...
```

```text
/login phala
/model phala/openai/gpt-oss-20b
```

Config: `/phala-settings` · Attestation status: `/attestation`

Interchangeable with `pi-provider-redpill` — both share the same verified
protocol core. If you operate your own private-ai-gateway, use the neutral
[`@phala/pi-provider-aci`](https://www.npmjs.com/package/@phala/pi-provider-aci) instead.

[private-ai-gateway]: https://github.com/Dstack-TEE/private-ai-gateway