import type { PluginModule } from "@opencode-ai/plugin";
import { REDPILL_ACI_PROFILE } from "@phala/aci-provider/profiles";
import { createOpenCodeAciPlugin } from "@phala/opencode-provider-aci";

export const RedpillProviderPlugin = createOpenCodeAciPlugin({
  profile: REDPILL_ACI_PROFILE,
});

const plugin: PluginModule = {
  id: "opencode-provider-redpill",
  server: RedpillProviderPlugin,
};

export default plugin;
