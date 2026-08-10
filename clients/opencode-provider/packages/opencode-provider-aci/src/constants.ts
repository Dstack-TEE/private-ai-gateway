// Module-level identity + env-driven configuration shared across the provider's
// modules. The identity values are live bindings populated by
// `applyProviderProfile()` (called once by the factory entry point), so branded
// shells (phala-cloud, ...) get their own provider id, env names, and default
// endpoint without touching the protocol code.

import { DEFAULT_PROFILE, profile, resolveProfile, type ProviderProfile } from "./profile.ts";
import { existsSync, readFileSync } from "node:fs";
import os from "node:os";
import { join } from "node:path";

// --- Identity (live bindings; see applyProviderProfile) ---
export let PROVIDER_ID = DEFAULT_PROFILE.providerId;
export let API_KEY_ENV = DEFAULT_PROFILE.apiKeyEnv;
export let DEFAULT_BASE_URL = DEFAULT_PROFILE.defaultBaseUrl;
export let LOG_PREFIX = DEFAULT_PROFILE.logPrefix;

export const PROVIDER_VERSION = "0.1.0";

/** Apply a resolved brand profile. Idempotent; call once before registering.
 *  Also updates profile()'s current profile so modules reading identity
 *  values through profile() (env prefix, aliases, oauth, fallback catalog)
 *  see the brand, not the neutral defaults. */
export function applyProviderProfile(patch: Partial<ProviderProfile> | undefined): void {
  const merged = resolveProfile(patch);
  PROVIDER_ID = merged.providerId;
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

/** Base URL from the environment only: {PREFIX}_{CLOUD_API_PREFIX|BASE_URL|
 *  CLOUD_BASE_URL} or the brand's legacy aliases. Undefined when unset. */
export function getEnvBaseUrl(): string | undefined {
  const p = profile();
  const prefixed = firstEnv(
    `${p.envPrefix}_CLOUD_API_PREFIX`,
    `${p.envPrefix}_BASE_URL`,
    `${p.envPrefix}_CLOUD_BASE_URL`,
  );
  return prefixed || firstEnv(...(p.baseUrlAliases ?? []));
}

/** Read the base URL for the current profile: environment, then the profile
 *  default. */
export function getBaseUrl(): string {
  return getEnvBaseUrl() || profile().defaultBaseUrl || DEFAULT_BASE_URL;
}

/** Read the inference API key from the environment ({PREFIX}_LLM_API_KEY or
 *  brand aliases). OAuth-stored credentials are captured separately by the
 *  auth loader (see core.ts); this is the env fallback. */
export function getEnvApiKey(): string {
  const p = profile();
  return firstEnv(p.apiKeyEnv, ...(p.apiKeyAliases ?? [])) ?? "";
}

/** Read the credential opencode stored for this provider via
 *  `opencode auth login` ($XDG_DATA_HOME/opencode/auth.json or
 *  ~/.local/share/opencode/auth.json). Used for startup model discovery and
 *  ACI artifact fetches before the auth loader has run. Returns "" when
 *  absent or expired. */
export function getStoredApiKey(): string {
  try {
    const dataHome = process.env.XDG_DATA_HOME?.trim() || join(os.homedir(), ".local", "share");
    const path = join(dataHome, "opencode", "auth.json");
    if (!existsSync(path)) return "";
    const parsed = JSON.parse(readFileSync(path, "utf8")) as Record<string, unknown>;
    const entry = parsed[PROVIDER_ID];
    if (!entry || typeof entry !== "object") return "";
    const rec = entry as Record<string, unknown>;
    if (rec.type === "oauth" && typeof rec.access === "string") {
      const expires = rec.expires;
      if (typeof expires === "number" && Number.isFinite(expires) && expires <= Date.now()) {
        return "";
      }
      return rec.access;
    }
    if (rec.type === "api" && typeof rec.key === "string") return rec.key;
  } catch {
    // Unreadable storage; fall through to "".
  }
  return "";
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
