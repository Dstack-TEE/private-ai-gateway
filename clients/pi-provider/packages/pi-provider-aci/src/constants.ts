// Module-level identity + env-driven configuration shared across the provider's
// modules. The identity values are live bindings populated by
// `applyProviderProfile()` (called once by the factory entry point), so branded
// shells (redpill, phala-cloud, ...) get their own provider id, env names,
// default endpoint, and footer key without touching the protocol code.

import { DEFAULT_PROFILE, profile, type ProviderProfile } from "./profile.ts";

// --- Identity (live bindings; see applyProviderProfile) ---
export let PROVIDER_ID = DEFAULT_PROFILE.providerId;
export let FOOTER_STATUS_KEY = DEFAULT_PROFILE.footerKey;
export let API_KEY_ENV = DEFAULT_PROFILE.apiKeyEnv;
export let DEFAULT_BASE_URL = DEFAULT_PROFILE.defaultBaseUrl;
export let LOG_PREFIX = DEFAULT_PROFILE.logPrefix;

export const PROVIDER_VERSION = "0.2.0";

/** Apply a resolved brand profile. Idempotent; call once before registering. */
export function applyProviderProfile(patch: Partial<ProviderProfile> | undefined): void {
  const merged = { ...DEFAULT_PROFILE, ...patch };
  PROVIDER_ID = merged.providerId;
  FOOTER_STATUS_KEY = merged.footerKey;
  API_KEY_ENV = merged.apiKeyEnv;
  DEFAULT_BASE_URL = merged.defaultBaseUrl;
  LOG_PREFIX = merged.logPrefix;
}

function firstEnv(...names: (string | undefined)[]): string | undefined {
  for (const name of names) {
    if (!name) continue;
    const value = process.env[name]?.trim();
    if (value) return value;
  }
  return undefined;
}

/** Read the base URL for the current profile: {PREFIX}_{API_PREFIX|BASE_URL|CLOUD_BASE_URL},
 *  or the brand's legacy aliases, then the profile default. */
export function getBaseUrl(): string {
  const p = profile();
  const prefixed = firstEnv(
    `${p.envPrefix}_CLOUD_API_PREFIX`,
    `${p.envPrefix}_BASE_URL`,
    `${p.envPrefix}_CLOUD_BASE_URL`,
  );
  const aliased = firstEnv(...(p.baseUrlAliases ?? []));
  return prefixed || aliased || p.defaultBaseUrl || DEFAULT_BASE_URL;
}

// Build a gateway-root URL (no trailing /v1) for ACI endpoints
// (/aci/receipts, /aci/attestation, /aci/sessions). The inference base URL is
// `<root>/v1`; ACI endpoints hang off the same host.
export function getGatewayRoot(baseUrl: string = getBaseUrl()): string {
  return baseUrl.replace(/\/v\d+\/?$/, "").replace(/\/+$/, "");
}

export function buildModelsUrl(baseUrl: string = getBaseUrl()): string {
  const base = baseUrl.replace(/\/+$/, "");
  return base.endsWith("/v1") ? `${base}/models` : `${base}/v1/models`;
}

export function buildReceiptUrl(receiptId: string, baseUrl: string = getBaseUrl()): string {
  return `${getGatewayRoot(baseUrl)}/v1/aci/receipts/${encodeURIComponent(receiptId)}`;
}

export function buildAttestationUrl(nonce: string, baseUrl: string = getBaseUrl()): string {
  return `${getGatewayRoot(baseUrl)}/v1/aci/attestation?nonce=${encodeURIComponent(nonce)}`;
}

export function buildSessionUrl(sessionId: string, baseUrl: string = getBaseUrl()): string {
  return `${getGatewayRoot(baseUrl)}/v1/aci/sessions/${encodeURIComponent(sessionId)}`;
}

// ACI response headers attached to every inference response.
export const HEADER_RECEIPT_ID = "x-receipt-id";
export const HEADER_ACI_IDENTITY = "x-aci-identity";
export const HEADER_ACI_KEYSET_DIGEST = "x-aci-keyset-digest";

export const DEFAULT_DISCOVERY_TIMEOUT_MS = 5000;
export const DEFAULT_RECEIPT_FETCH_TIMEOUT_MS = 8000;
export const DEFAULT_ATTESTATION_FETCH_TIMEOUT_MS = 8000;

// Attestation freshness: re-fetch when the cached report's stale_after has
// passed, or after this fallback TTL if the report lacked freshness info.
export const ATTESTATION_FALLBACK_TTL_MS = 30 * 60 * 1000;