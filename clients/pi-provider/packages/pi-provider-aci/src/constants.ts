// Protocol constants and URL helpers shared across provider instances.

import { DEFAULT_PROFILE, type ProviderProfile } from "./profile.ts";

export const LOG_PREFIX = DEFAULT_PROFILE.logPrefix;

export const PROVIDER_VERSION = "0.4.0";

function firstEnv(env: NodeJS.ProcessEnv, ...names: (string | undefined)[]): string | undefined {
  for (const name of names) {
    if (!name) continue;
    const value = env[name]?.trim();
    if (value) return value;
  }
  return undefined;
}

/** Read the base URL for a profile: {PREFIX}_{API_PREFIX|BASE_URL|CLOUD_BASE_URL},
 *  or the brand's legacy aliases, then the profile default. */
export function getBaseUrl(
  providerProfile: ProviderProfile = DEFAULT_PROFILE,
  env: NodeJS.ProcessEnv = process.env,
): string {
  const p = providerProfile;
  const prefixed = firstEnv(
    env,
    `${p.envPrefix}_CLOUD_API_PREFIX`,
    `${p.envPrefix}_BASE_URL`,
    `${p.envPrefix}_CLOUD_BASE_URL`,
  );
  const aliased = firstEnv(env, ...(p.baseUrlAliases ?? []));
  return prefixed || aliased || p.defaultBaseUrl;
}

// Build a gateway-root URL (no trailing /v1) for ACI endpoints
// (/aci/receipts, /aci/attestation, /aci/sessions). The inference base URL is
// `<root>/v1`; ACI endpoints hang off the same host.
export function getGatewayRoot(baseUrl: string = DEFAULT_PROFILE.defaultBaseUrl): string {
  return baseUrl.replace(/\/v\d+\/?$/, "").replace(/\/+$/, "");
}

export function buildModelsUrl(baseUrl: string = DEFAULT_PROFILE.defaultBaseUrl): string {
  const base = baseUrl.replace(/\/+$/, "");
  return base.endsWith("/v1") ? `${base}/models` : `${base}/v1/models`;
}

export function buildReceiptUrl(
  receiptId: string,
  baseUrl: string = DEFAULT_PROFILE.defaultBaseUrl,
): string {
  return `${getGatewayRoot(baseUrl)}/v1/aci/receipts/${encodeURIComponent(receiptId)}`;
}

export function buildSessionUrl(
  sessionId: string,
  baseUrl: string = DEFAULT_PROFILE.defaultBaseUrl,
): string {
  return `${getGatewayRoot(baseUrl)}/v1/aci/sessions/${encodeURIComponent(sessionId)}`;
}

// ACI response headers attached to every inference response.
export const HEADER_RECEIPT_ID = "x-receipt-id";

export const DEFAULT_DISCOVERY_TIMEOUT_MS = 5000;
export const DEFAULT_RECEIPT_FETCH_TIMEOUT_MS = 8000;
export const DEFAULT_ATTESTATION_FETCH_TIMEOUT_MS = 8000;
