import type { PluginModule } from "@opencode-ai/plugin";
import type { AciFetch } from "@phala/aci-provider";
import {
  resolvePhalaCloudApiBaseURL,
  startPhalaCloudAccountAuthorization,
} from "@phala/aci-provider/phala-cloud";
import { PHALA_CLOUD_ACI_PROFILE } from "@phala/aci-provider/profiles";
import { createOpenCodeAciPlugin, type OpenCodeAciAuthMethod } from "@phala/opencode-provider-aci";

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
      const authorization = await startPhalaCloudAccountAuthorization({
        baseURL,
        clientId,
        ...(fetch ? { fetch } : {}),
      });
      return {
        url: authorization.verificationURI,
        instructions: `Approve the device login with code ${authorization.userCode}`,
        method: "auto",
        async callback() {
          const credential = await authorization.complete();
          return {
            type: "success",
            key: credential.apiKey,
            ...(credential.metadata ? { metadata: credential.metadata } : {}),
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
