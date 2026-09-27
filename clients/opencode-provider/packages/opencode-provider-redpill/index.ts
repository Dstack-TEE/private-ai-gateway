import type { PluginModule } from "@opencode-ai/plugin";
import type { Plugin } from "@opencode/plugin";
import { REDPILL_ACI_PROFILE } from "@phala/aci-provider/profiles";
import { createOpenCodeAciPlugin, loadOpenCodeAciV2Plugin } from "@phala/opencode-provider-aci";

const server = createOpenCodeAciPlugin({
  profile: REDPILL_ACI_PROFILE,
});

/** OpenCode 1 server-plugin entrypoint. */
export const RedPillProviderPlugin = server;

let definition: Plugin.Plugin | undefined;

const plugin: PluginModule & Plugin.Plugin = {
  id: "opencode-provider-redpill",
  async setup(context) {
    definition ??= await loadOpenCodeAciV2Plugin({
      id: "opencode-provider-redpill",
      profile: REDPILL_ACI_PROFILE,
    });
    return definition.setup(context);
  },
  server,
};

export default plugin;
