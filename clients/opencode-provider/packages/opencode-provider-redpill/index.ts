import type { PluginModule } from "@opencode-ai/plugin";
import { createOpenCodeAciPlugin } from "@phala/opencode-provider-aci";

export const RedpillProviderPlugin = createOpenCodeAciPlugin({
  providerId: "redpill",
  label: "Redpill AI",
  defaultBaseURL: "https://tee.redpill.ai/v1",
  apiKeyEnv: "REDPILL_LLM_API_KEY",
  apiKeyAliases: ["REDPILL_API_KEY"],
  envPrefix: "REDPILL",
  logPrefix: "[redpill]",
  baseURLAliases: ["REDPILL_CLOUD_API_PREFIX", "REDPILL_BASE_URL"],
  catalog: [
    {
      id: "deepseek/deepseek-v4-flash",
      name: "DeepSeek V4 Flash",
      family: "deepseek",
      reasoning: true,
      thinkingFormat: "openai",
      toolCall: true,
      temperature: true,
      input: ["text"],
      output: ["text"],
      cost: { input: 0.2, output: 0.4, cacheRead: 0.2, cacheWrite: 0 },
      contextWindow: 1_048_576,
      maxOutputTokens: 65_536,
    },
    {
      id: "z-ai/glm-5.2",
      name: "Z.AI GLM 5.2",
      family: "glm",
      reasoning: true,
      thinkingFormat: "openai",
      toolCall: true,
      temperature: true,
      input: ["text"],
      output: ["text"],
      cost: { input: 1.4, output: 4.4, cacheRead: 0.5, cacheWrite: 0 },
      contextWindow: 1_048_576,
      maxOutputTokens: 131_072,
    },
  ],
});

const plugin: PluginModule = {
  id: "opencode-provider-redpill",
  server: RedpillProviderPlugin,
};

export default plugin;
