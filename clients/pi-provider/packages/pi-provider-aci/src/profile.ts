// Provider identity/profile.
//
// The core is a vendor-neutral client of the private-ai-gateway "ACI"
// protocol. A single default profile ("aci") is defined here; branded
// distributions (`pi-provider-redpill`, `pi-provider-phala-cloud`, ...) build
// `createProvider(profile)` with their own identity. The core never enumerates
// vendors.
//
// profile.ts owns the *identity* values (provider id, env names, default
// endpoint, footer key, fallback catalog). Everything protocol-y (attestation,
// TLS SPKI pinning, model discovery, config layering) lives elsewhere and is
// identity-agnostic.

export interface ProviderProfile {
  /** Provider id registered in pi. */
  providerId: string;
  /** Human-facing label for the settings UI / status. */
  label: string;
  /** Default gateway base URL (branded shells set this; core is operator-set). */
  defaultBaseUrl: string;
  /** Env var for the LLM/inference API key. */
  apiKeyEnv: string;
  /** Prefix for config env vars: {PREFIX}_BASE_URL, {PREFIX}_IS_TEE_ONLY, ... */
  envPrefix: string;
  /** Footer/status bar key. */
  footerKey: string;
  /** Log prefix, e.g. "[aci]". */
  logPrefix: string;
  /** Fallback model catalog used when discovery has no API key. */
  fallbackModels: Array<{
    id: string;
    name: string;
    reasoning: boolean;
    input: ("text" | "image")[];
    cost: { input: number; output: number; cacheRead: number; cacheWrite: number };
    contextWindow: number;
    maxTokens: number;
  }>;
  /** Optional legacy env-var aliases for the base URL / API key (brand
   *  backward-compat). */
  baseUrlAliases?: string[];
  apiKeyAliases?: string[];
  /** Optional OAuth login block (device flow or otherwise). Branded shells
   *  that support /login register this; the core passes it through to pi's
   *  registerProvider `oauth` config and, when set, `resolveApiKey()` first
   *  reads the stored credential (auth.json) before falling back to the env
   *  var. The shell owns the flow implementation; the core only transports
   *  the config. */
  oauth?: AciOAuthConfig;
}

/** OAuth config the core forwards to pi's registerProvider `oauth` block. */
export interface AciOAuthConfig {
  /** Display name shown in `/login`. */
  name: string;
  login(
    callbacks: import("@earendil-works/pi-ai").OAuthLoginCallbacks,
  ): Promise<import("@earendil-works/pi-ai").OAuthCredentials>;
  refreshToken(
    credentials: import("@earendil-works/pi-ai").OAuthCredentials,
  ): Promise<import("@earendil-works/pi-ai").OAuthCredentials>;
  getApiKey(credentials: import("@earendil-works/pi-ai").OAuthCredentials): string;
}

export const DEFAULT_PROFILE: ProviderProfile = {
  providerId: "aci",
  label: "Private AI Gateway",
  defaultBaseUrl: "",
  apiKeyEnv: "ACI_LLM_API_KEY",
  envPrefix: "ACI",
  footerKey: "aci",
  logPrefix: "[aci]",
  fallbackModels: [],
};

/** Resolve a (possibly partial) profile over the neutral defaults. */
export function resolveProfile(patch: Partial<ProviderProfile> | undefined): ProviderProfile {
  return { ...DEFAULT_PROFILE, ...stripEmpty(patch) };
}

function stripEmpty<T extends Record<string, unknown>>(patch: T | undefined): T {
  if (!patch) return {} as T;
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(patch)) {
    if (v === undefined) continue;
    out[k] = v;
  }
  return out as T;
}
