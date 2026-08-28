# @phala/pi-provider-aci

Vendor-neutral Pi provider for an Attested Confidential Inference (ACI)
gateway. It establishes an instance-scoped verified connection with
`@phala/aci-verifier`, injects the scoped transport into Pi, discovers models,
verifies every inference receipt before stream completion, and exposes
attestation, receipt, and session audit commands.

```bash
pi install npm:@phala/pi-provider-aci
export ACI_BASE_URL=https://gateway.example/v1
pi
```

In Pi, store the gateway key and save a default model through the native UI:

```text
/login aci
# paste the API key and wait for the footer to show aci-verified
/model
# search for aci/, select a model, and press Ctrl+S
```

Pi owns all persistence: credentials are stored in
`~/.pi/agent/auth.json`, refreshed catalogs in
`~/.pi/agent/models-store.json`, and a default selected with `Ctrl+S` in
`~/.pi/agent/settings.json`. The cached catalog remains available offline.
`ACI_API_KEY` is also supported for the current process, but Pi does not copy
environment variables into its credential store.

The provider fails closed when workload or channel verification fails. For a
production reviewed-release claim, configure `ACI_ACCEPTED_COMPOSE_HASHES` with
the comma-separated compose hashes published by the deployment operator.
`/aci-receipt [id]` displays or re-verifies a recorded exchange's signed
receipt, exact wire body hashes, and cited attested session. `/aci-session <id>`
fetches the public session artifact over the pinned connection and validates it
locally; it does not require an inference API key.

See the [full client documentation](https://github.com/Dstack-TEE/private-ai-gateway/tree/main/clients/pi-provider)
for configuration, trust boundaries, and branded packages.
