import type { PluginModule } from "@opencode-ai/plugin";
import {
  createPhalaCloudAccountAuth,
  resolvePhalaCloudApiBaseURL,
} from "@phala/aci-provider/phala-cloud";
import { PHALA_CLOUD_ACI_PROFILE } from "@phala/aci-provider/profiles";
import { createOpenCodeAciPlugin, createOpenCodeAciV2Plugin } from "@phala/opencode-provider-aci";

export const PhalaProviderPlugin = createOpenCodeAciPlugin({
  profile: PHALA_CLOUD_ACI_PROFILE,
  accountAuth: createPhalaCloudAccountAuth({
    baseURL: resolvePhalaCloudApiBaseURL(),
    clientId: "opencode",
  }),
});

/** OpenCode V2 plugin definition for Phala Cloud ACI. */
export const PhalaProviderPluginV2 = createOpenCodeAciV2Plugin({
  id: "opencode-provider-phala-cloud",
  profile: PHALA_CLOUD_ACI_PROFILE,
  accountAuth: createPhalaCloudAccountAuth({
    baseURL: resolvePhalaCloudApiBaseURL(),
    clientId: "opencode",
  }),
});

const plugin: PluginModule & typeof PhalaProviderPluginV2 = {
  ...PhalaProviderPluginV2,
  server: PhalaProviderPlugin,
};

export default plugin;
