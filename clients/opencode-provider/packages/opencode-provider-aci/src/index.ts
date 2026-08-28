import {
  createAciProvider,
  resolveAciProviderConfig,
  resolveAciProviderProfile,
  type AciModel,
  type AciFetch,
  type AciProvider,
  type AciProviderConfigInput,
  type AciProviderProfile,
} from "@phala/aci-provider";
import type { AuthHook, Config, Plugin, PluginModule, PluginOptions } from "@opencode-ai/plugin";

import { createAciInspectTool } from "./inspect.ts";

const OPENAI_COMPATIBLE_PACKAGE = "@ai-sdk/openai-compatible";

type OpenCodeProviderConfig = NonNullable<Config["provider"]>[string];
type OpenCodeCommandConfig = NonNullable<Config["command"]>[string];

export interface OpenCodeModelConfig {
  name: string;
  family?: string;
  attachment: boolean;
  reasoning: boolean;
  temperature: boolean;
  tool_call: boolean;
  interleaved: false | { field: "reasoning_content" };
  cost: {
    input: number;
    output: number;
    cache_read: number;
    cache_write: number;
  };
  limit: { context: number; output: number };
  modalities: {
    input: ("text" | "audio" | "image" | "video" | "pdf")[];
    output: ("text" | "audio" | "image" | "video" | "pdf")[];
  };
  variants?: Record<string, Record<string, unknown>>;
}

type AciReceiptOptions = NonNullable<AciProviderConfigInput["receipts"]>;

export type OpenCodeAciPluginOptions = Omit<AciProviderConfigInput, "receipts"> & {
  receipts?: Omit<AciReceiptOptions, "verification">;
};
export type OpenCodeAciAuthMethod = AuthHook["methods"][number];

export interface CreateOpenCodeAciPluginOptions {
  profile?: Partial<AciProviderProfile>;
  defaults?: OpenCodeAciPluginOptions;
  authMethods?: readonly OpenCodeAciAuthMethod[];
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function pluginConfig(options: PluginOptions | undefined): OpenCodeAciPluginOptions {
  if (!options) return {};
  return {
    baseURL: options.baseURL,
    ...(isRecord(options.models) ? { models: options.models } : {}),
    ...(isRecord(options.trust) ? { trust: options.trust } : {}),
    ...(isRecord(options.receipts) ? { receipts: options.receipts } : {}),
  };
}

export function mapOpenCodeModel(model: AciModel): OpenCodeModelConfig {
  return {
    name: model.name,
    ...(model.family ? { family: model.family } : {}),
    attachment: model.input.some((modality) => modality !== "text"),
    reasoning: model.reasoning,
    temperature: model.temperature,
    tool_call: model.toolCall,
    interleaved: model.reasoning ? { field: "reasoning_content" } : false,
    cost: {
      input: model.cost.input,
      output: model.cost.output,
      cache_read: model.cost.cacheRead,
      cache_write: model.cost.cacheWrite,
    },
    limit: { context: model.contextWindow, output: model.maxOutputTokens },
    modalities: { input: [...model.input], output: [...model.output] },
    ...(model.thinkingFormat === "qwen"
      ? {
          variants: {
            off: { enable_thinking: false },
            high: { enable_thinking: true },
          },
        }
      : {}),
  };
}

function modelMap(models: readonly AciModel[]): Record<string, OpenCodeModelConfig> {
  return Object.fromEntries(models.map((model) => [model.id, mapOpenCodeModel(model)]));
}

function registerInspectCommands(config: Config, providerId: string, toolName: string): void {
  const command = (
    action: "attestation" | "receipts" | "receipt" | "session",
    description: string,
    id?: "optional" | "required",
  ): OpenCodeCommandConfig => ({
    description,
    template: [
      `Call the ${toolName} tool exactly once with action "${action}".`,
      ...(id === "optional"
        ? ['If "$1" is empty, omit id; otherwise pass "$1" exactly as id.']
        : id === "required"
          ? ['Pass "$1" exactly as id.']
          : []),
      "Return the tool output verbatim without commentary and do not call any other tool.",
    ].join(" "),
  });
  const commands: Record<string, OpenCodeCommandConfig> = {
    [`${providerId}-attestation`]: command(
      "attestation",
      `Show the verified ${providerId} ACI workload identity`,
    ),
    [`${providerId}-receipts`]: command("receipts", `List retained ${providerId} ACI receipts`),
    [`${providerId}-receipt`]: command(
      "receipt",
      `Verify the latest or selected ${providerId} ACI receipt`,
      "optional",
    ),
    [`${providerId}-session`]: command("session", `Verify a ${providerId} ACI session`, "required"),
  };

  config.command ??= {};
  for (const [name, value] of Object.entries(commands)) config.command[name] ??= value;
}

export function createOpenCodeAciPlugin({
  profile: profileInput = {},
  defaults = {},
  authMethods = [],
}: CreateOpenCodeAciPluginOptions = {}): Plugin {
  const profile = resolveAciProviderProfile(profileInput);
  const methods = [...authMethods];
  if (!methods.some((method) => method.type === "api")) {
    methods.push({ type: "api", label: `${profile.label} API key` });
  }

  return async (_input, rawOptions) => {
    const options = pluginConfig(rawOptions);
    let active: AciProvider | undefined;
    let blockedReason = "ACI provider is still verifying the gateway";

    const secureFetch: AciFetch = (request, init) => {
      if (!active) return Promise.reject(new Error(blockedReason));
      return active.fetch(request, init);
    };
    const inspectToolName =
      profile.providerId === "aci" ? "aci_inspect" : `${profile.providerId}_aci_inspect`;

    return {
      tool: {
        [inspectToolName]: createAciInspectTool(() => active, profile.label),
      },
      async config(config) {
        const baseURL = options.baseURL ?? defaults.baseURL;
        const configuredBaseURL =
          typeof baseURL === "string" && baseURL ? baseURL : profile.defaultBaseURL;
        config.provider ??= {};
        const owned: OpenCodeProviderConfig = {
          name: profile.label,
          npm: OPENAI_COMPATIBLE_PACKAGE,
          env: [profile.apiKeyEnv],
          options: {
            ...(configuredBaseURL ? { baseURL: configuredBaseURL } : {}),
            fetch: secureFetch,
          },
          models: {},
        };
        config.provider[profile.providerId] = owned;
        registerInspectCommands(config, profile.providerId, inspectToolName);

        const previous = active;
        active = undefined;
        let candidate: AciProvider | undefined;
        try {
          const resolved = resolveAciProviderConfig(profile, {
            ...defaults,
            ...options,
            baseURL,
            models: { ...defaults.models, ...options.models },
            trust: { ...defaults.trust, ...options.trust },
            receipts: {
              ...defaults.receipts,
              ...options.receipts,
              verification: "response",
            },
          });
          candidate = createAciProvider(resolved);
          owned.options = { ...owned.options, baseURL: resolved.baseURL, fetch: secureFetch };
          await candidate.connect();
          const models = await candidate.discoverModels();
          owned.models = modelMap(models);
          await previous?.close();
          active = candidate;
          blockedReason = "ACI provider is unavailable";
        } catch (error) {
          blockedReason = `ACI provider blocked: ${error instanceof Error ? error.message : String(error)}`;
          await Promise.allSettled([candidate?.close(), previous?.close()]);
          throw error;
        }
      },
      auth: {
        provider: profile.providerId,
        loader: async (getAuth) => {
          const auth = await getAuth();
          return {
            fetch: secureFetch,
            ...(auth?.type === "api" ? { apiKey: auth.key } : {}),
          };
        },
        methods,
      },
      async "chat.params"(request, output) {
        if (request.model.providerID !== profile.providerId) return;
        const model = active?.models().find((item) => item.id === request.model.id);
        if (!model) return;
        if (model.thinkingFormat === "qwen" && output.options.enable_thinking === undefined) {
          output.options.enable_thinking = true;
        }
        if (model.thinkingFormat === "off") {
          delete output.options.enable_thinking;
          delete output.options.reasoningEffort;
        }
      },
      async dispose() {
        const provider = active;
        active = undefined;
        await provider?.close();
      },
    };
  };
}

export const AciProviderPlugin = createOpenCodeAciPlugin();

const plugin: PluginModule = {
  id: "@phala/opencode-provider-aci",
  server: AciProviderPlugin,
};

export default plugin;
