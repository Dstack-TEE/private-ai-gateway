import type { Api, Model } from "@earendil-works/pi-ai";
import {
  discoverAciModelCatalog,
  inferThinkingFormat as inferAciThinkingFormat,
  mapAciModel,
  type AciModel,
  type AciServerModel,
} from "@phala/aci-provider";

import { toAciProviderConfig, type AciCloudConfig } from "./config.ts";
import { DEFAULT_DISCOVERY_TIMEOUT_MS, LOG_PREFIX } from "./constants.ts";
import { DEFAULT_PROFILE, type ProviderProfile } from "./profile.ts";

export type AciPiModel = Omit<Model<"openai-completions">, "api" | "provider" | "baseUrl">;

interface InferredThinking {
  reasoning: boolean;
  format: "qwen" | "openai" | "off";
  maxTokensField: "max_tokens" | "max_completion_tokens";
  supportsReasoningEffort: boolean;
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
): AciPiModel {
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
): AciPiModel | null {
  const mapped = mapAciModel(model, toAciProviderConfig(config));
  return mapped ? toPiModel(mapped, config.models.thinkingFormat) : null;
}

export interface DiscoverAciModelsOptions {
  timeoutMs?: number;
  baseUrl?: string;
  fetch?: typeof globalThis.fetch;
  logPrefix?: string;
}

export interface DiscoverAciModelsResult {
  models: AciPiModel[];
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
      config: toAciProviderConfig({ ...config, baseUrl: options.baseUrl ?? config.baseUrl }),
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

export function fallbackModels(providerProfile: ProviderProfile = DEFAULT_PROFILE): AciPiModel[] {
  return providerProfile.catalog.map((model) => toPiModel(model, "auto"));
}

export type { AciServerModel };
export type AnyModel = Model<Api>;
