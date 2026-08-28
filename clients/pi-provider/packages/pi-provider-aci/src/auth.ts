import { envApiKeyAuth, type ApiKeyAuth } from "@earendil-works/pi-ai";

import type { ProviderProfile } from "./profile.ts";

export function createApiKeyAuth(profile: ProviderProfile): ApiKeyAuth {
  const auth = envApiKeyAuth(profile.apiKeyAuth?.name ?? `${profile.label} API key`, [
    profile.apiKeyEnv,
  ]);
  return profile.apiKeyAuth?.login ? { ...auth, login: profile.apiKeyAuth.login } : auth;
}
