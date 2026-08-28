import {
  envApiKeyAuth,
  type ApiKeyAuth,
  type ApiKeyCredential,
  type ProviderAuthInteraction,
} from "@earendil-works/pi-ai";
import type { AccountApiKeyAuth } from "@phala/aci-provider";

import type { ProviderProfile } from "./profile.ts";

async function loginWithAccount(
  account: AccountApiKeyAuth,
  interaction: ProviderAuthInteraction,
): Promise<ApiKeyCredential> {
  const authorization = await account.start({ signal: interaction.signal });
  if (authorization.presentation.type === "device_code") {
    interaction.notify({
      type: "device_code",
      userCode: authorization.presentation.userCode,
      verificationUri: authorization.url,
      ...(authorization.presentation.intervalSeconds === undefined
        ? {}
        : { intervalSeconds: authorization.presentation.intervalSeconds }),
      ...(authorization.presentation.expiresInSeconds === undefined
        ? {}
        : { expiresInSeconds: authorization.presentation.expiresInSeconds }),
    });
  } else {
    interaction.notify({
      type: "auth_url",
      url: authorization.url,
      ...(authorization.instructions ? { instructions: authorization.instructions } : {}),
    });
  }
  const credential = await authorization.complete({
    signal: interaction.signal,
    onProgress: (message) => interaction.notify({ type: "progress", message }),
  });
  return { type: "api_key", key: credential.apiKey };
}

export function createApiKeyAuth(
  profile: ProviderProfile,
  account?: AccountApiKeyAuth,
): ApiKeyAuth {
  const apiKey = envApiKeyAuth(`${profile.label} API key`, [profile.apiKeyEnv]);
  if (!account) return apiKey;
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
          { id: "account", label: account.label },
          { id: "api-key", label: apiKey.name },
        ],
      });
      return method === "account"
        ? loginWithAccount(account, interaction)
        : apiKeyLogin(interaction);
    },
  };
}
