/**
 * pi-provider-phala-cloud — Phala Cloud branded distribution of the
 * vendor-neutral private-ai-gateway (ACI) Pi provider.
 *
 * This package is a thin skin: it imports the core `@phala/pi-provider-aci` and
 * registers it with the Phala Cloud identity (provider id, endpoint, env vars,
 * device login). All protocol logic — attestation, TLS SPKI
 * pinning, model discovery — lives in the core.
 *
 * Usage:
 *   pi install npm:pi-provider-phala-cloud
 *   # /login phala (or set PHALA_AI_API_KEY), then /model phala/<model-id>
 */
import {
  createPhalaCloudAccountAuth,
  resolvePhalaCloudApiBaseURL,
} from "@phala/aci-provider/phala-cloud";
import { PHALA_CLOUD_ACI_PROFILE } from "@phala/aci-provider/profiles";
import { createProvider } from "@phala/pi-provider-aci";

export default createProvider({
  profile: PHALA_CLOUD_ACI_PROFILE,
  footerKey: "phala",
  accountAuth: createPhalaCloudAccountAuth({
    baseURL: resolvePhalaCloudApiBaseURL(),
    clientId: "pi",
    includeAccountMetadata: false,
  }),
});

export { createProvider } from "@phala/pi-provider-aci";
export { PROVIDER_VERSION } from "@phala/pi-provider-aci";
