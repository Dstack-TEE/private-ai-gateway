# `@phala/aci-provider`

Framework-neutral provider kernel for ACI gateways. It owns verified connection
lifecycle, live model discovery, TEE-only filtering, model capability mapping,
bounded receipt history, optional response-completion receipt verification, and
content-addressed session inspection.

Host adapters such as Pi and OpenCode supply their native configuration and UI.
Applications normally install a host adapter rather than this package directly.
Shared Redpill and Phala Cloud profiles are exported from
`@phala/aci-provider/profiles`. Phala Cloud's account API and device
authorization flow are isolated under `@phala/aci-provider/phala-cloud`; they
are not part of the ACI protocol or verifier.

```ts
import {
  createAciProvider,
  resolveAciProviderConfig,
  resolveAciProviderProfile,
} from "@phala/aci-provider";

const profile = resolveAciProviderProfile({
  defaultBaseURL: "https://gateway.example.com/v1",
});
const provider = createAciProvider(resolveAciProviderConfig(profile));

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

Model discovery uses the verified connection but sends no inference API key;
the gateway's `/v1/models` catalog is public. Host adapters own credentials and
attach them only to inference requests.

`provider.receipts()` returns the bounded in-process exchange history, newest
first. `provider.verifyReceipt()` verifies the latest exchange when no id is
given, while `provider.verifySession(id)` fetches the public session artifact
over the pinned connection and validates its content address, API version,
validity window, and evidence digest.
