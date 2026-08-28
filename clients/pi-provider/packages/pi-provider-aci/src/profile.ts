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
import type { ApiKeyAuth } from "@earendil-works/pi-ai";

export interface AciApiKeyAuthConfig {
  /** Login option shown by Pi. Defaults to `<label> API key`. */
  name?: string;
  /** Optional branded login flow. The neutral default prompts for an API key. */
  login?: ApiKeyAuth["login"];
}

export interface ProviderProfile extends AciProviderProfile {
  /** Footer/status bar key. */
  footerKey: string;
  /** Optional branded API-key login flow, such as Phala Cloud device authorization. */
  apiKeyAuth?: AciApiKeyAuthConfig;
}

export const DEFAULT_PROFILE: ProviderProfile = {
  ...DEFAULT_ACI_PROVIDER_PROFILE,
  footerKey: "aci",
};

/** Resolve a (possibly partial) profile over the neutral defaults. */
export function resolveProfile(patch: Partial<ProviderProfile> | undefined): ProviderProfile {
  const { footerKey = DEFAULT_PROFILE.footerKey, apiKeyAuth, ...shared } = patch ?? {};
  const profile = resolveAciProviderProfile(shared);
  return {
    ...profile,
    footerKey,
    ...(apiKeyAuth ? { apiKeyAuth } : {}),
  };
}
