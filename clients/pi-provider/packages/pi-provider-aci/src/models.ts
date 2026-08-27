import type { Api, Model } from "@earendil-works/pi-ai";
import type { ProviderModelConfig } from "@earendil-works/pi-coding-agent";
import {
  discoverAciModelCatalog,
  inferThinkingFormat as inferAciThinkingFormat,
  mapAciModel,
  type AciModel,
  type AciProviderConfig,
  type AciServerModel,
} from "@phala/aci-provider";

import type { AciCloudConfig } from "./config.ts";
import { DEFAULT_DISCOVERY_TIMEOUT_MS, LOG_PREFIX } from "./constants.ts";
import { DEFAULT_PROFILE, type ProviderProfile } from "./profile.ts";

interface InferredThinking {
  reasoning: boolean;
  format: "qwen" | "openai" | "off";
  maxTokensField: "max_tokens" | "max_completion_tokens";
  supportsReasoningEffort: boolean;
}

function providerConfig(config: AciCloudConfig): AciProviderConfig {
  return {
    baseURL: config.baseUrl,
    models: config.models,
    trust: config.trust,
    receipts: { verification: "on-demand", historySize: 32 },
  };
}

export function inferThinkingFormat(modelId: string): InferredThinking {
  const format = inferAciThinkingFormat(modelId);
  if (format === "qwen") {
    return {
      reasoning: true,
      format,
      maxTokensField: "max_tokens",
      supportsReasoningEffort: false,
    };
  }
  if (format === "openai") {
    return {
      reasoning: true,
      format,
      maxTokensField: modelId.toLowerCase().includes("gpt-oss")
        ? "max_completion_tokens"
        : "max_tokens",
      supportsReasoningEffort: true,
    };
  }
  return {
    reasoning: false,
    format,
    maxTokensField: "max_tokens",
    supportsReasoningEffort: false,
  };
}

function toPiModel(
  model: AciModel,
  configuredFormat: AciCloudConfig["models"]["thinkingFormat"],
): ProviderModelConfig {
  const inferred = inferThinkingFormat(model.id);
  const thinking: InferredThinking =
    configuredFormat === "auto"
      ? inferred
      : {
          reasoning: model.reasoning,
          format: model.thinkingFormat,
          maxTokensField:
            model.thinkingFormat === "openai" ? "max_completion_tokens" : "max_tokens",
          supportsReasoningEffort: model.thinkingFormat === "openai",
        };
  return {
    id: model.id,
    name: model.name,
    reasoning: model.reasoning,
    input: model.input.includes("image") ? ["text", "image"] : ["text"],
    cost: model.cost,
    contextWindow: model.contextWindow,
    maxTokens: model.maxOutputTokens,
    compat: {
      thinkingFormat: thinking.format === "off" ? "openai" : thinking.format,
      maxTokensField: thinking.maxTokensField,
      supportsReasoningEffort: thinking.supportsReasoningEffort,
      supportsStrictMode: false,
      supportsUsageInStreaming: true,
    },
  };
}

export function mapAciServerModel(
  model: AciServerModel,
  config: AciCloudConfig,
): ProviderModelConfig | null {
  const mapped = mapAciModel(model, providerConfig(config));
  return mapped ? toPiModel(mapped, config.models.thinkingFormat) : null;
}

export interface DiscoverAciModelsOptions {
  timeoutMs?: number;
  baseUrl?: string;
  fetch?: typeof globalThis.fetch;
  logPrefix?: string;
}

export interface DiscoverAciModelsResult {
  models: ProviderModelConfig[];
  raw: AciServerModel[];
}

export async function discoverAciModels(
  apiKey: string,
  config: AciCloudConfig,
  options: DiscoverAciModelsOptions = {},
): Promise<DiscoverAciModelsResult> {
  try {
    const catalog = await discoverAciModelCatalog({
      ...(apiKey ? { apiKey } : {}),
      config: providerConfig({ ...config, baseUrl: options.baseUrl ?? config.baseUrl }),
      fetch: options.fetch ?? globalThis.fetch,
      timeoutMs: options.timeoutMs ?? DEFAULT_DISCOVERY_TIMEOUT_MS,
    });
    return {
      raw: [...catalog.raw],
      models: catalog.models.map((model) => toPiModel(model, config.models.thinkingFormat)),
    };
  } catch (error) {
    console.error(`${options.logPrefix ?? LOG_PREFIX} model discovery failed:`, error);
    return { models: [], raw: [] };
  }
}

export function fallbackModels(
  providerProfile: ProviderProfile = DEFAULT_PROFILE,
): ProviderModelConfig[] {
  return providerProfile.fallbackModels.map((model) => {
    const thinking = inferThinkingFormat(model.id);
    return {
      ...model,
      compat: {
        thinkingFormat: thinking.format === "off" ? "openai" : thinking.format,
        maxTokensField: thinking.maxTokensField,
        supportsReasoningEffort: thinking.supportsReasoningEffort,
        supportsStrictMode: false,
        supportsUsageInStreaming: true,
      },
    } as ProviderModelConfig;
  });
}

export type { AciServerModel };
export type AnyModel = Model<Api>;
