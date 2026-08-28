export interface AciProviderProfile {
  providerId: string;
  label: string;
  defaultBaseURL: string;
  apiKeyEnv: string;
  envPrefix: string;
  logPrefix: string;
  acceptedComposeHashes?: readonly string[];
  acceptedSessionIds?: readonly string[];
}

export const DEFAULT_ACI_PROVIDER_PROFILE: AciProviderProfile = {
  providerId: "aci",
  label: "Private AI Gateway",
  defaultBaseURL: "",
  apiKeyEnv: "ACI_API_KEY",
  envPrefix: "ACI",
  logPrefix: "[aci]",
};

export function resolveAciProviderProfile(
  patch: Partial<AciProviderProfile> = {},
): AciProviderProfile {
  return {
    ...DEFAULT_ACI_PROVIDER_PROFILE,
    ...Object.fromEntries(Object.entries(patch).filter(([, value]) => value !== undefined)),
  };
}
