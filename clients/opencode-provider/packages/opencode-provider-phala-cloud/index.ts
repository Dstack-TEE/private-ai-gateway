import type { PluginModule } from "@opencode-ai/plugin";
import type { AciFetch } from "@phala/aci-provider";
import {
  fetchPhalaCloudAccount,
  resolvePhalaCloudApiBaseURL,
  startPhalaCloudDeviceAuthorization,
} from "@phala/aci-provider/phala-cloud";
import { PHALA_CLOUD_ACI_PROFILE } from "@phala/aci-provider/profiles";
import { createOpenCodeAciPlugin, type OpenCodeAciAuthMethod } from "@phala/opencode-provider-aci";

const ACCOUNT_METADATA_TIMEOUT_MS = 5_000;

export interface CreatePhalaCloudDeviceAuthMethodOptions {
  label?: string;
  baseURL: string;
  clientId: string;
  fetch?: AciFetch;
}

export function createPhalaCloudDeviceAuthMethod({
  label = "Phala Cloud account",
  baseURL,
  clientId,
  fetch,
}: CreatePhalaCloudDeviceAuthMethodOptions): OpenCodeAciAuthMethod {
  return {
    type: "oauth",
    label,
    async authorize() {
      const authorization = await startPhalaCloudDeviceAuthorization({
        baseURL,
        clientId,
        ...(fetch ? { fetch } : {}),
      });
      return {
        url: authorization.verificationURI,
        instructions: `Approve the device login with code ${authorization.userCode}`,
        method: "auto",
        async callback() {
          const token = await authorization.poll();
          const metadata: Record<string, string> = {};
          if (token.keyId !== undefined) metadata.keyId = String(token.keyId);
          try {
            const account = await fetchPhalaCloudAccount({
              baseURL,
              apiKey: token.accessToken,
              signal: AbortSignal.timeout(ACCOUNT_METADATA_TIMEOUT_MS),
              ...(fetch ? { fetch } : {}),
            });
            if (account.username) metadata.username = account.username;
            if (account.workspaceName) metadata.workspaceName = account.workspaceName;
            if (account.workspaceSlug) metadata.workspaceSlug = account.workspaceSlug;
          } catch {
            // Account metadata is optional; the issued inference key remains valid.
          }
          return {
            type: "success",
            key: token.accessToken,
            ...(Object.keys(metadata).length > 0 ? { metadata } : {}),
          };
        },
      };
    },
  };
}

export const PhalaProviderPlugin = createOpenCodeAciPlugin({
  profile: PHALA_CLOUD_ACI_PROFILE,
  authMethods: [
    createPhalaCloudDeviceAuthMethod({
      baseURL: resolvePhalaCloudApiBaseURL(),
      clientId: "opencode",
    }),
  ],
});

const plugin: PluginModule = {
  id: "opencode-provider-phala-cloud",
  server: PhalaProviderPlugin,
};

export default plugin;
