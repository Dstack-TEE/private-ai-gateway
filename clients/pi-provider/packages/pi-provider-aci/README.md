# @phala/pi-provider-aci

Vendor-neutral Pi provider for an Attested Confidential Inference (ACI)
gateway. It verifies the gateway workload and pins its attested TLS key before
sending model traffic, discovers the gateway's models, and verifies every
inference receipt before the model turn finishes.

## Install

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

Pi stores the credential, catalog, and default model in its own files.
`ACI_API_KEY` also works for the current process, but Pi does not save it.

## Configure

| Environment variable          | Effect                                                                           |
| ----------------------------- | -------------------------------------------------------------------------------- |
| `ACI_BASE_URL`                | Gateway endpoint. Required.                                                      |
| `ACI_ACCEPTED_COMPOSE_HASHES` | Comma-separated reviewed compose hashes. Set these for a reviewed-release claim. |
| `ACI_ACCEPTED_SESSION_IDS`    | Comma-separated audited session IDs that requests may use.                       |

`/aci-settings`, `/aci-attestation`, `/aci-receipts`, `/aci-receipt [id]`, and
`/aci-session <id>` show the settings, attestation, retained receipts, and
sessions. Verification runs automatically; the commands only display evidence
or rerun an audit.

## Build a branded provider

`createProvider()` registers the provider under your brand:

```ts
import { createProvider } from "@phala/pi-provider-aci";
export default createProvider({
  profile: {
    providerId: "my-brand",
    label: "My Brand",
    defaultBaseURL: "https://gateway.example/v1",
    apiKeyEnv: "MY_AI_API_KEY",
    envPrefix: "MY",
    logPrefix: "[my-brand]",
    acceptedComposeHashes: ["<reviewed-sha256-app-compose>"],
  },
  footerKey: "my-brand",
});
```

A product with an account-to-API-key flow can also pass a shared
`accountAuth`. The adapter maps it into `/login` next to manual API-key entry.

## Learn more

- [Coding-agent integrations](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/clients/coding-agents.md#pi):
  persistence, local receipt history, and branded packages.
- [Client architecture](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/clients/architecture.md):
  what the verifier checks, when receipts are verified, and release
  acceptance.
