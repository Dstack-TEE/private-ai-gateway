import type { AciProviderProfile } from "./profile.ts";

export const REDPILL_ACI_PROFILE = {
  providerId: "redpill",
  label: "RedPill AI",
  defaultBaseURL: "https://tee.redpill.ai/v1",
  apiKeyEnv: "REDPILL_AI_API_KEY",
  envPrefix: "REDPILL",
  logPrefix: "[redpill]",
  baseURLAliases: ["REDPILL_CLOUD_API_PREFIX", "REDPILL_BASE_URL"],
} as const satisfies AciProviderProfile;

export const PHALA_CLOUD_ACI_PROFILE = {
  providerId: "phala",
  label: "Phala Cloud",
  defaultBaseURL: "https://inference.phala.com/v1",
  apiKeyEnv: "PHALA_AI_API_KEY",
  envPrefix: "PHALA",
  logPrefix: "[phala]",
  baseURLAliases: ["PHALA_CLOUD_API_PREFIX", "PHALA_BASE_URL", "PHALA_CLOUD_BASE_URL"],
} as const satisfies AciProviderProfile;
