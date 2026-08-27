import {
  createAciProvider,
  resolveAciApiKey,
  resolveAciProviderConfig,
  resolveAciProviderProfile,
  type AciModel,
  type AciFetch,
  type AciProvider,
  type AciProviderConfigInput,
  type AciProviderProfile,
} from "@phala/aci-provider";
import {
  fetchPhalaCloudAccount,
  startPhalaCloudDeviceAuthorization,
} from "@phala/aci-provider/phala-cloud";
import type { AuthHook, Config, Plugin, PluginModule, PluginOptions } from "@opencode-ai/plugin";

const OPENAI_COMPATIBLE_PACKAGE = "@ai-sdk/openai-compatible";

interface OpenCodeProviderConfig {
  name?: string;
  env?: string[];
  npm?: string;
  options?: Record<string, unknown>;
  models?: Record<string, OpenCodeModelConfig>;
}

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

export type OpenCodeAciPluginOptions = AciProviderConfigInput;
export type OpenCodeAciAuthMethod = AuthHook["methods"][number];

export interface CreateOpenCodeAciPluginOptions {
  profile?: Partial<AciProviderProfile>;
  defaults?: AciProviderConfigInput;
  authMethods?: readonly OpenCodeAciAuthMethod[];
}

export interface CreatePhalaCloudAuthMethodOptions {
  label?: string;
  baseURL: string;
  clientId: string;
  fetch?: AciFetch;
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

export function createPhalaCloudAuthMethod({
  label = "Phala Cloud account",
  baseURL,
  clientId,
  fetch,
}: CreatePhalaCloudAuthMethodOptions): OpenCodeAciAuthMethod {
  return {
    type: "oauth",
    label,
    async authorize() {
      const authorization = await startPhalaCloudDeviceAuthorization({
        baseURL,
        clientId,
        ...(fetch ? { fetch } : {}),
      });
      return {
        url: authorization.verificationURI,
        instructions: `Approve the device login with code ${authorization.userCode}`,
        method: "auto",
        async callback() {
          const token = await authorization.poll();
          const metadata: Record<string, string> = {};
          if (token.keyId !== undefined) metadata.keyId = String(token.keyId);
          try {
            const account = await fetchPhalaCloudAccount({
              baseURL,
              apiKey: token.accessToken,
              ...(fetch ? { fetch } : {}),
            });
            if (account.username) metadata.username = account.username;
            if (account.workspaceName) metadata.workspaceName = account.workspaceName;
            if (account.workspaceSlug) metadata.workspaceSlug = account.workspaceSlug;
          } catch {
            // Account metadata is optional; the issued inference key remains valid.
          }
          return {
            type: "success",
            key: token.accessToken,
            ...(Object.keys(metadata).length > 0 ? { metadata } : {}),
          };
        },
      };
    },
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

function providerFromConfig(
  config: Config,
  providerId: string,
): OpenCodeProviderConfig | undefined {
  return config.provider?.[providerId] as OpenCodeProviderConfig | undefined;
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

    return {
      async config(config) {
        const existing = providerFromConfig(config, profile.providerId);
        const existingOptions = existing?.options ?? {};
        const baseURL = options.baseURL ?? existingOptions.baseURL ?? defaults.baseURL;
        config.provider ??= {};
        const owned: OpenCodeProviderConfig = {
          ...existing,
          name: profile.label,
          npm: OPENAI_COMPATIBLE_PACKAGE,
          env: Array.from(new Set([profile.apiKeyEnv, ...(profile.apiKeyAliases ?? [])])),
          options: {
            ...existingOptions,
            baseURL:
              (typeof baseURL === "string" && baseURL) ||
              profile.defaultBaseURL ||
              "https://invalid.invalid/v1",
            fetch: secureFetch,
          },
          models: {
            ...modelMap(profile.catalog),
            ...existing?.models,
          },
        };
        config.provider[profile.providerId] = owned;

        const previous = active;
        active = undefined;
        let candidate: AciProvider | undefined;
        try {
          const apiKey = resolveAciApiKey(profile);
          const resolved = resolveAciProviderConfig(profile, {
            ...defaults,
            ...options,
            baseURL,
            models: { ...defaults.models, ...options.models },
            trust: { ...defaults.trust, ...options.trust },
            receipts: {
              verification: "response",
              ...defaults.receipts,
              ...options.receipts,
            },
          });
          candidate = createAciProvider({ profile, config: resolved });
          owned.options = { ...owned.options, baseURL: resolved.baseURL, fetch: secureFetch };
          await candidate.connect();
          const models = await candidate.discoverModels(apiKey);
          owned.models = { ...modelMap(models), ...existing?.models };
          active = candidate;
          blockedReason = "ACI provider is unavailable";
          await previous?.close();
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
        await active?.close();
        active = undefined;
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
