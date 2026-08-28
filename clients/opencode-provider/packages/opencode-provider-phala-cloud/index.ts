import type { PluginModule } from "@opencode-ai/plugin";
import {
  createPhalaCloudAccountAuth,
  resolvePhalaCloudApiBaseURL,
} from "@phala/aci-provider/phala-cloud";
import { PHALA_CLOUD_ACI_PROFILE } from "@phala/aci-provider/profiles";
import { createOpenCodeAciPlugin } from "@phala/opencode-provider-aci";

export const PhalaProviderPlugin = createOpenCodeAciPlugin({
  profile: PHALA_CLOUD_ACI_PROFILE,
  accountAuth: createPhalaCloudAccountAuth({
    baseURL: resolvePhalaCloudApiBaseURL(),
    clientId: "opencode",
  }),
});

const plugin: PluginModule = {
  id: "opencode-provider-phala-cloud",
  server: PhalaProviderPlugin,
};

export default plugin;
