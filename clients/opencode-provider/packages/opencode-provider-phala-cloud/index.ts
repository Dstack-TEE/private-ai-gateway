import type { PluginModule } from "@opencode-ai/plugin";
import { createDeviceAuthMethod, createOpenCodeAciPlugin } from "@phala/opencode-provider-aci";

const cloudApiBaseURL =
  process.env.PHALA_CLOUD_API_BASE_URL?.trim().replace(/\/+$/, "") || "https://cloud-api.phala.com";

export const PhalaProviderPlugin = createOpenCodeAciPlugin({
  profile: {
    providerId: "phala",
    label: "Phala Cloud",
    defaultBaseURL: "https://inference.phala.com/v1",
    apiKeyEnv: "PHALA_LLM_API_KEY",
    envPrefix: "PHALA",
    logPrefix: "[phala]",
    baseURLAliases: ["PHALA_CLOUD_API_PREFIX", "PHALA_BASE_URL", "PHALA_CLOUD_BASE_URL"],
    catalog: [
      {
        id: "phala/qwen3.5-27b",
        name: "Phala Qwen3.5 27B",
        family: "qwen",
        reasoning: true,
        thinkingFormat: "qwen",
        toolCall: true,
        temperature: true,
        input: ["text"],
        output: ["text"],
        cost: { input: 0.3, output: 2.4, cacheRead: 0, cacheWrite: 0 },
        contextWindow: 262_000,
        maxOutputTokens: 8_192,
      },
    ],
  },
  authMethods: [
    createDeviceAuthMethod({
      label: "Phala Cloud account",
      baseURL: cloudApiBaseURL,
      clientId: "opencode",
      scope: "redpill:api-key",
    }),
  ],
});

const plugin: PluginModule = {
  id: "opencode-provider-phala-cloud",
  server: PhalaProviderPlugin,
};

export default plugin;
