import type { PluginModule } from "@opencode-ai/plugin";
import { resolvePhalaCloudApiBaseURL } from "@phala/aci-provider/phala-cloud";
import { PHALA_CLOUD_ACI_PROFILE } from "@phala/aci-provider/profiles";
import { createOpenCodeAciPlugin, createPhalaCloudAuthMethod } from "@phala/opencode-provider-aci";

export const PhalaProviderPlugin = createOpenCodeAciPlugin({
  profile: PHALA_CLOUD_ACI_PROFILE,
  authMethods: [
    createPhalaCloudAuthMethod({
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
