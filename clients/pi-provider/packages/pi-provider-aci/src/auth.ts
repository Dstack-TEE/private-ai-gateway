import {
  envApiKeyAuth,
  type ApiKeyAuth,
  type ApiKeyCredential,
  type OAuthAuth,
  type OAuthCredential,
  type ProviderAuthInteraction,
} from "@earendil-works/pi-ai";
import type { AccountApiKeyAuth } from "@phala/aci-provider";

import type { ProviderProfile } from "./profile.ts";

// Device-issued account keys do not rotate through an OAuth refresh grant.
// Re-arm the validity window instead so `Models` never treats the stored
// credential as expired (and never needs to re-login).
const ACCOUNT_CREDENTIAL_TTL_MS = 30 * 24 * 60 * 60 * 1000;

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

/**
 * Expose a product account flow (device code / authorization URL that issues
 * one inference API key) as Pi's account sign-in (`auth.oauth`), so the
 * provider appears in the "Sign in with an account" login list next to the
 * subscription OAuth providers. The issued API key rides in a custom
 * `apiKey` credential field; `toAuth` derives request auth from it.
 */
export function createAccountOAuthAuth(
  profile: ProviderProfile,
  account: AccountApiKeyAuth,
): OAuthAuth {
  return {
    name: `${profile.label} account`,
    loginLabel: `Sign in with your ${profile.label} account`,
    async login(interaction: ProviderAuthInteraction): Promise<OAuthCredential> {
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
      return {
        type: "oauth",
        access: "",
        refresh: "",
        expires: Date.now() + ACCOUNT_CREDENTIAL_TTL_MS,
        apiKey: credential.apiKey,
      };
    },
    async refresh(credential) {
      return { ...credential, expires: Date.now() + ACCOUNT_CREDENTIAL_TTL_MS };
    },
    async toAuth(credential) {
      const apiKey =
        typeof credential.apiKey === "string" && credential.apiKey.length > 0
          ? credential.apiKey
          : undefined;
      if (!apiKey) {
        throw new Error(`${profile.label} account credential is missing its API key`);
      }
      return { apiKey };
    },
  };
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
