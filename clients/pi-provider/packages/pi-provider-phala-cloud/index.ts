/**
 * pi-provider-phala-cloud — Phala Cloud branded distribution of the
 * vendor-neutral private-ai-gateway (ACI) Pi provider.
 *
 * This package is a thin skin: it imports the core `@phala/pi-provider-aci` and
 * registers it with the Phala Cloud identity (provider id, endpoint, env vars,
 * fallback catalog, OAuth login). All protocol logic — attestation, TLS SPKI
 * pinning, model discovery — lives in the core.
 *
 * Usage:
 *   pi install npm:pi-provider-phala-cloud
 *   # /login phala (or set PHALA_LLM_API_KEY), then /model phala/<model-id>
 */
import type { OAuthCredentials, OAuthLoginCallbacks } from "@earendil-works/pi-ai";
import {
  fetchPhalaCloudAccount,
  resolvePhalaCloudApiBaseURL,
  startPhalaCloudDeviceAuthorization,
} from "@phala/aci-provider/phala-cloud";
import { PHALA_CLOUD_ACI_PROFILE } from "@phala/aci-provider/profiles";
import { createProvider } from "@phala/pi-provider-aci";

// RFC 8628 device authorization against Phala Cloud issues a Confidential AI
// key rather than a general cloud token. The key does not expire and cannot be
// refreshed, so `expires` is set far in the future and refreshToken() throws.
async function loginPhalaDeviceFlow(callbacks: OAuthLoginCallbacks): Promise<OAuthCredentials> {
  const cloudApi = resolvePhalaCloudApiBaseURL();
  const authorization = await startPhalaCloudDeviceAuthorization({
    baseURL: cloudApi,
    clientId: "pi",
    signal: callbacks.signal,
  });
  callbacks.onDeviceCode({
    userCode: authorization.userCode,
    verificationUri: authorization.verificationURI,
    intervalSeconds: authorization.interval,
    expiresInSeconds: authorization.expiresIn,
  });
  const token = await authorization.poll({
    signal: callbacks.signal,
    onProgress: callbacks.onProgress,
  });

  const credentials: OAuthCredentials = {
    refresh: "",
    access: token.accessToken,
    expires: Date.now() + 100 * 365 * 24 * 60 * 60 * 1000,
  };
  if (token.keyId !== undefined) {
    credentials.redpill_key_id = token.keyId;
  }

  // Best-effort display metadata from the LLM-key self endpoint.
  try {
    const account = await fetchPhalaCloudAccount({
      baseURL: cloudApi,
      apiKey: token.accessToken,
      signal: callbacks.signal,
    });
    if (account.username) credentials.username = account.username;
    if (account.workspaceSlug) credentials.workspace_slug = account.workspaceSlug;
    if (account.workspaceName) credentials.workspace_name = account.workspaceName;
  } catch {
    // Metadata is display-only; login still succeeds without it.
  }
  return credentials;
}

export default createProvider({
  ...PHALA_CLOUD_ACI_PROFILE,
  footerKey: "phala",
  oauth: {
    name: "Phala Cloud",
    login: loginPhalaDeviceFlow,
    // Redpill LLM keys do not expire and have no rotation endpoint; a dead
    // key surfaces as a 401 and the user re-runs /login to mint a new one.
    refreshToken: () => {
      throw new Error("Phala LLM keys cannot be refreshed; run /login phala again");
    },
    getApiKey: (credentials) => credentials.access,
  },
});

export { createProvider } from "@phala/pi-provider-aci";
export { PROVIDER_VERSION } from "@phala/pi-provider-aci";
