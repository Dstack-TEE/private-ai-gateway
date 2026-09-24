import type { PluginModule } from "@opencode-ai/plugin";
import type { Plugin } from "@opencode/plugin";
import {
  createPhalaCloudAccountAuth,
  resolvePhalaCloudApiBaseURL,
} from "@phala/aci-provider/phala-cloud";
import { PHALA_CLOUD_ACI_PROFILE } from "@phala/aci-provider/profiles";
import { createOpenCodeAciPlugin, createOpenCodeAciV2Plugin } from "@phala/opencode-provider-aci";

function accountAuth() {
  return createPhalaCloudAccountAuth({
    baseURL: resolvePhalaCloudApiBaseURL(),
    clientId: "opencode",
  });
}

const server = createOpenCodeAciPlugin({
  profile: PHALA_CLOUD_ACI_PROFILE,
  accountAuth: accountAuth(),
});

let definition: Plugin.Plugin | undefined;

const plugin: PluginModule & Plugin.Plugin = {
  id: "opencode-provider-phala-cloud",
  async setup(context) {
    definition ??= await createOpenCodeAciV2Plugin({
      id: "opencode-provider-phala-cloud",
      profile: PHALA_CLOUD_ACI_PROFILE,
      accountAuth: accountAuth(),
    });
    return definition.setup(context);
  },
  server,
};

export default plugin;
