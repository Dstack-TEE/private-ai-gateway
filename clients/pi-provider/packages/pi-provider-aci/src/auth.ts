import { envApiKeyAuth, type ApiKeyAuth } from "@earendil-works/pi-ai";

import type { ProviderProfile } from "./profile.ts";

export function createApiKeyAuth(profile: ProviderProfile): ApiKeyAuth {
  const apiKey = envApiKeyAuth(`${profile.label} API key`, [profile.apiKeyEnv]);
  const account = profile.apiKeyAuth;
  if (!account?.login) return apiKey;
  const accountLogin = account.login;
  const apiKeyLogin = apiKey.login;
  if (!apiKeyLogin) return apiKey;

  return {
    ...apiKey,
    name: profile.label,
    async login(interaction) {
      const method = await interaction.prompt({
        type: "select",
        message: `Log in to ${profile.label}`,
        options: [
          { id: "account", label: account.name ?? `${profile.label} account` },
          { id: "api-key", label: apiKey.name },
        ],
      });
      return method === "account" ? accountLogin(interaction) : apiKeyLogin(interaction);
    },
  };
}
