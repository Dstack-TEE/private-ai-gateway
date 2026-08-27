# `@phala/aci-provider`

Framework-neutral provider kernel for ACI gateways. It owns verified connection
lifecycle, live model discovery, TEE-only filtering, model capability mapping,
bounded receipt history, and optional response-completion receipt verification.

Host adapters such as Pi and OpenCode supply their native configuration and UI.
Applications normally install a host adapter rather than this package directly.
Shared Redpill and Phala Cloud profiles are exported from
`@phala/aci-provider/profiles`. Phala Cloud's account API and RFC 8628 device
flow are isolated under `@phala/aci-provider/phala-cloud`; they are not part of
the ACI protocol or verifier.

```ts
import {
  createAciProvider,
  resolveAciProviderConfig,
  resolveAciProviderProfile,
} from "@phala/aci-provider";

const profile = resolveAciProviderProfile({
  defaultBaseURL: "https://gateway.example.com/v1",
});
const provider = createAciProvider({
  profile,
  config: resolveAciProviderConfig(profile),
});

await provider.connect();
const response = await provider.fetch("https://gateway.example.com/v1/chat/completions", {
  method: "POST",
  headers: { Authorization: `Bearer ${process.env.ACI_API_KEY}` },
  body: JSON.stringify({ model: "model-id", messages: [] }),
});
```

The provider fails closed: model traffic is sent only after workload attestation
and TLS SPKI binding succeed. Set `receipts.verification` to `"response"` to
make a response stream finish only after its signed receipt and cited session
have verified.
