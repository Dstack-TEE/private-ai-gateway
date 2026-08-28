# `@phala/aci-provider`

Framework-neutral provider kernel for ACI gateways. It owns verified connection
lifecycle, live model discovery, TEE-only filtering, model capability mapping,
bounded receipt history, optional response-completion receipt verification, and
content-addressed session inspection. It also exposes structured inspection
results and a shared text formatter, so host adapters do not duplicate ACI
audit semantics.

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

Host integrations can use one structured inspection contract for status,
attestation, receipt history, receipt verification, and session verification:

```ts
import { formatAciInspection, inspectAciProvider } from "@phala/aci-provider";

const result = await inspectAciProvider(provider, { action: "receipt" });
console.log(formatAciInspection(result));
```

Products that exchange account authorization for an inference API key implement
the host-neutral `AccountApiKeyAuth` contract. It describes the browser/device
step and returns one API key plus optional metadata; host adapters present it
and persist the result through native auth APIs. Phala Cloud provides the
shared factory directly:

```ts
import { createPhalaCloudAccountAuth } from "@phala/aci-provider/phala-cloud";

const accountAuth = createPhalaCloudAccountAuth({
  baseURL: "https://cloud-api.phala.com",
  clientId: "my-agent",
});
```

This product authentication contract is separate from ACI verification.
Redpill currently has no account authorization implementation and remains
API-key-only.
