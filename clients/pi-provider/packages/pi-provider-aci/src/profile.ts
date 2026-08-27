// Provider identity/profile.
//
// The core is a vendor-neutral client of the private-ai-gateway "ACI"
// protocol. A single default profile ("aci") is defined here; branded
// distributions (`pi-provider-redpill`, `pi-provider-phala-cloud`, ...) build
// `createProvider(profile)` with their own identity. The core never enumerates
// vendors.
//
// profile.ts adds Pi-specific fields to the shared provider profile. Everything
// protocol-y (attestation, TLS SPKI pinning, model discovery, config layering)
// lives elsewhere and is identity-agnostic.

import {
  DEFAULT_ACI_PROVIDER_PROFILE,
  resolveAciProviderProfile,
  type AciProviderProfile,
} from "@phala/aci-provider";

export interface ProviderProfile extends AciProviderProfile {
  /** Footer/status bar key. */
  footerKey: string;
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
  ...DEFAULT_ACI_PROVIDER_PROFILE,
  apiKeyAliases: ["ACI_LLM_API_KEY"],
  footerKey: "aci",
};

/** Resolve a (possibly partial) profile over the neutral defaults. */
export function resolveProfile(patch: Partial<ProviderProfile> | undefined): ProviderProfile {
  const { footerKey = DEFAULT_PROFILE.footerKey, oauth, ...shared } = patch ?? {};
  const profile = resolveAciProviderProfile(shared);
  return {
    ...profile,
    ...(profile.providerId === DEFAULT_PROFILE.providerId && profile.apiKeyAliases === undefined
      ? { apiKeyAliases: DEFAULT_PROFILE.apiKeyAliases }
      : {}),
    footerKey,
    ...(oauth ? { oauth } : {}),
  };
}
