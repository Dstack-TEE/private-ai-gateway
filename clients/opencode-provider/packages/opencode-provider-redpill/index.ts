import type { PluginModule } from "@opencode-ai/plugin";
import { REDPILL_ACI_PROFILE } from "@phala/aci-provider/profiles";
import { createOpenCodeAciPlugin, createOpenCodeAciV2Plugin } from "@phala/opencode-provider-aci";

export const RedPillProviderPlugin = createOpenCodeAciPlugin({
  profile: REDPILL_ACI_PROFILE,
});

/** OpenCode V2 plugin definition for RedPill ACI. */
export const RedPillProviderPluginV2 = createOpenCodeAciV2Plugin({
  id: "opencode-provider-redpill",
  profile: REDPILL_ACI_PROFILE,
});

const plugin: PluginModule & typeof RedPillProviderPluginV2 = {
  ...RedPillProviderPluginV2,
  server: RedPillProviderPlugin,
};

export default plugin;
