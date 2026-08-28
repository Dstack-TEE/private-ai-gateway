/**
 * pi-provider-redpill — RedPill AI branded distribution of the
 * vendor-neutral private-ai-gateway (ACI) Pi provider.
 *
 * This package is a thin skin: it imports the core `@phala/pi-provider-aci` and
 * registers it with the RedPill identity (provider id, endpoint, env vars).
 * All protocol logic — attestation, TLS SPKI pinning,
 * model discovery — lives in the core.
 *
 * Usage:
 *   pi install npm:pi-provider-redpill
 *   export REDPILL_AI_API_KEY=...
 *   # /model redpill/<model-id>
 */
import { REDPILL_ACI_PROFILE } from "@phala/aci-provider/profiles";
import { createProvider } from "@phala/pi-provider-aci";

export default createProvider({
  profile: REDPILL_ACI_PROFILE,
  footerKey: "redpill",
});

export { createProvider } from "@phala/pi-provider-aci";
export { PROVIDER_VERSION } from "@phala/pi-provider-aci";
