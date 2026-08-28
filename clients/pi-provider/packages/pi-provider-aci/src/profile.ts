// Provider identity/profile.
//
// The core is a vendor-neutral client of the private-ai-gateway "ACI"
// protocol. A single default profile ("aci") is defined here; branded
// distributions (`pi-provider-redpill`, `pi-provider-phala-cloud`, ...) pass a
// profile to `createProvider()`. The core never enumerates vendors.
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
}

export const DEFAULT_PROFILE: ProviderProfile = {
  ...DEFAULT_ACI_PROVIDER_PROFILE,
  footerKey: "aci",
};

/** Resolve a (possibly partial) profile over the neutral defaults. */
export function resolveProfile(patch: Partial<ProviderProfile> | undefined): ProviderProfile {
  const { footerKey = DEFAULT_PROFILE.footerKey, ...shared } = patch ?? {};
  const profile = resolveAciProviderProfile(shared);
  return {
    ...profile,
    footerKey,
  };
}
