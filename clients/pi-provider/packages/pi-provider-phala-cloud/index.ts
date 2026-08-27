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
 *   # /login phala (or set PHALA_LLM_API_KEY), then /model phala/<model-id>
 */
import type { ApiKeyCredential, ProviderAuthInteraction } from "@earendil-works/pi-ai";
import {
  resolvePhalaCloudApiBaseURL,
  startPhalaCloudDeviceAuthorization,
} from "@phala/aci-provider/phala-cloud";
import { PHALA_CLOUD_ACI_PROFILE } from "@phala/aci-provider/profiles";
import { createProvider } from "@phala/pi-provider-aci";

// Phala Cloud device authorization issues a Confidential AI API key, so Pi
// stores the result as an API-key credential instead of inventing an OAuth
// token lifecycle.
async function loginPhalaDeviceFlow(
  interaction: ProviderAuthInteraction,
): Promise<ApiKeyCredential> {
  const cloudApi = resolvePhalaCloudApiBaseURL();
  const authorization = await startPhalaCloudDeviceAuthorization({
    baseURL: cloudApi,
    clientId: "pi",
    signal: interaction.signal,
  });
  interaction.notify({
    type: "device_code",
    userCode: authorization.userCode,
    verificationUri: authorization.verificationURI,
    intervalSeconds: authorization.interval,
    expiresInSeconds: authorization.expiresIn,
  });
  const token = await authorization.poll({
    signal: interaction.signal,
    onProgress: (message) => interaction.notify({ type: "progress", message }),
  });
  return { type: "api_key", key: token.accessToken };
}

export default createProvider({
  ...PHALA_CLOUD_ACI_PROFILE,
  footerKey: "phala",
  apiKeyAuth: {
    name: "Phala Cloud account",
    login: loginPhalaDeviceFlow,
  },
});

export { createProvider } from "@phala/pi-provider-aci";
export { PROVIDER_VERSION } from "@phala/pi-provider-aci";
