# pi-provider-redpill

Attested AI for Pi, powered by [private-ai-gateway].

SoT brand skin for Redpill (**attested TLS / SPKI pinning** — prevention, not
receipt audit). Users install the **release artifact**, not this path:

```bash
pi install git:github.com/redpill-ai/pi-provider-redpill
# or
git clone https://github.com/redpill-ai/pi-provider-redpill
cd pi-provider-redpill && npm install && pi -e .
export REDPILL_LLM_API_KEY=...
```

API key only — **no OAuth** device flow in this brand.

Default gateway: `https://api.redpill.ai/v1` (`REDPILL_BASE_URL` to override).

```text
/model redpill/deepseek/deepseek-v4-flash
```

Config: `/redpill-settings` · Attestation: `/attestation`

Kernel + verifier changes land in the monorepo (`pi-provider-aci`,
`clients/verifier-ts`) and are packed into the artifact via
`make -C clients/pi-provider pack-redpill`.

[private-ai-gateway]: https://github.com/Dstack-TEE/private-ai-gateway
