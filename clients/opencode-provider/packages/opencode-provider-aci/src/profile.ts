// Provider identity/profile.
//
// The core is a vendor-neutral client of the private-ai-gateway "ACI"
// protocol. A single default profile ("aci") is defined here; branded
// distributions (`opencode-provider-phala-cloud`, ...) build
// `createProvider(profile)` with their own identity. The core never
// enumerates vendors.
//
// profile.ts owns the *identity* values (provider id, env names, default
// endpoint, fallback catalog). Everything protocol-y (attestation, TLS SPKI
// pinning, receipt verification, model discovery) lives elsewhere and is
// identity-agnostic.

export interface ProviderProfile {
  /** Provider id registered in opencode (config.provider key). */
  providerId: string;
  /** Human-facing label for the provider display name / status. */
  label: string;
  /** Default gateway base URL (branded shells set this; core is operator-set). */
  defaultBaseUrl: string;
  /** Env var for the LLM/inference API key. */
  apiKeyEnv: string;
  /** Prefix for config env vars: {PREFIX}_BASE_URL, {PREFIX}_IS_TEE_ONLY, ... */
  envPrefix: string;
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
  /** Optional OAuth login block (RFC 8628 device flow). Branded shells that
   *  support `opencode auth login` supply this; the core adapts it onto
   *  opencode's auth hook (authorize -> verification URL, callback -> poll).
   *  The shell owns the HTTP flow; the core only transports the config. */
  oauth?: AciOAuthConfig;
}

/** Credentials minted by a completed device-flow login. */
export interface AciOAuthCredentials {
  access: string;
  refresh: string;
  /** Absolute expiry (Unix ms). Keys that do not expire use a far-future value. */
  expires: number;
}

/** Device-flow session returned by the authorization endpoint (RFC 8628 §3.2). */
export interface AciDeviceFlowStart {
  deviceCode: string;
  userCode: string;
  verificationUri: string;
  intervalSeconds: number;
  expiresInSeconds: number;
}

/** OAuth config the core adapts onto opencode's `auth` hook. */
export interface AciOAuthConfig {
  /** Display name shown in `opencode auth login`. */
  name: string;
  /** Begin the device authorization flow (POST device/code). */
  startDeviceFlow(signal?: AbortSignal): Promise<AciDeviceFlowStart>;
  /** Poll the token endpoint until the user approves, the code expires, or
   *  the flow fails. Throws on failure/expiry. */
  pollDeviceFlow(start: AciDeviceFlowStart, signal?: AbortSignal): Promise<AciOAuthCredentials>;
}

export const DEFAULT_PROFILE: ProviderProfile = {
  providerId: "aci",
  label: "Private AI Gateway",
  defaultBaseUrl: "",
  apiKeyEnv: "ACI_LLM_API_KEY",
  envPrefix: "ACI",
  logPrefix: "[aci]",
  fallbackModels: [],
};

let current: ProviderProfile = DEFAULT_PROFILE;

/** Resolve a (possibly partial) profile over the neutral defaults. */
export function resolveProfile(patch: Partial<ProviderProfile> | undefined): ProviderProfile {
  current = { ...DEFAULT_PROFILE, ...stripEmpty(patch) };
  return current;
}

/** The currently active profile (set by the factory entry point). */
export function profile(): ProviderProfile {
  return current;
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
