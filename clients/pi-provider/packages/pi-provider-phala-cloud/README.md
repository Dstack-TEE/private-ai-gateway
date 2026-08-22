# pi-provider-phala-cloud

Phala Cloud Confidential AI for Pi, powered by [private-ai-gateway].

SoT brand skin for Phala Cloud (**attested TLS / SPKI pinning** — prevention,
not receipt audit). Users install the **release artifact**, not this path:

```bash
pi install git:github.com/Phala-Network/pi-provider-phala-cloud
# or
git clone https://github.com/Phala-Network/pi-provider-phala-cloud
cd pi-provider-phala-cloud && npm install && pi -e .
```

Auth: `/login phala` (RFC 8628 device flow) or `PHALA_LLM_API_KEY`.

```text
/login phala
/model phala/<model-id>
```

Config: `/phala-settings` · Attestation: `/attestation`

Kernel + verifier changes land in the monorepo (`pi-provider-aci`,
`clients/verifier-ts`) and are packed into the artifact via
`make -C clients/pi-provider pack-phala-cloud`.

[private-ai-gateway]: https://github.com/Dstack-TEE/private-ai-gateway
