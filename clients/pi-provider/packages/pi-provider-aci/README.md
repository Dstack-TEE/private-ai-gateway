# @phala/pi-provider-aci

Vendor-neutral Pi provider for an Attested Confidential Inference (ACI)
gateway. It establishes an instance-scoped verified connection with
`@phala/aci-verifier`, injects the scoped transport into Pi, discovers models,
verifies every inference receipt before stream completion, and exposes
attestation, receipt, and session audit commands.

```bash
pi install npm:@phala/pi-provider-aci
export ACI_BASE_URL=https://gateway.example/v1
export ACI_API_KEY=...
pi
```

The provider fails closed when workload or channel verification fails. For a
production reviewed-release claim, configure `ACI_ACCEPTED_COMPOSE_HASHES` with
the comma-separated compose hashes published by the deployment operator.
`/aci-receipt [id]` displays or re-verifies a recorded exchange's signed
receipt, exact wire body hashes, and cited attested session.

See the [full client documentation](https://github.com/Dstack-TEE/private-ai-gateway/tree/main/clients/pi-provider)
for configuration, trust boundaries, and branded packages.
