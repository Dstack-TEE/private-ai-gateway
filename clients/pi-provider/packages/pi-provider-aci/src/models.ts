import type { Api, Model } from "@earendil-works/pi-ai";
import {
  discoverAciModelCatalog,
  inferThinkingFormat as inferAciThinkingFormat,
  mapAciModel,
  type AciModel,
  type AciServerModel,
} from "@phala/aci-provider";

import { toAciProviderConfig, type AciCloudConfig } from "./config.ts";

export type AciPiModel = Omit<Model<"openai-completions">, "api" | "provider" | "baseUrl">;

interface InferredThinking {
  reasoning: boolean;
  format: "qwen" | "openai" | "off";
  maxTokensField: "max_tokens" | "max_completion_tokens";
  supportsReasoningEffort: boolean;
}

function piThinking(modelId: string, format: InferredThinking["format"]): InferredThinking {
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

export function inferThinkingFormat(modelId: string): InferredThinking {
  return piThinking(modelId, inferAciThinkingFormat(modelId));
}

function toPiModel(model: AciModel): AciPiModel {
  const thinking = piThinking(model.id, model.thinkingFormat);
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
  return mapped ? toPiModel(mapped) : null;
}

export interface DiscoverAciModelsOptions {
  timeoutMs?: number;
  baseUrl?: string;
  fetch?: typeof globalThis.fetch;
  signal?: AbortSignal;
}

export interface DiscoverAciModelsResult {
  models: AciPiModel[];
  raw: AciServerModel[];
}

export async function discoverAciModels(
  config: AciCloudConfig,
  options: DiscoverAciModelsOptions = {},
): Promise<DiscoverAciModelsResult> {
  const catalog = await discoverAciModelCatalog({
    config: toAciProviderConfig({ ...config, baseUrl: options.baseUrl ?? config.baseUrl }),
    fetch: options.fetch ?? globalThis.fetch,
    ...(options.signal ? { signal: options.signal } : {}),
    ...(options.timeoutMs !== undefined ? { timeoutMs: options.timeoutMs } : {}),
  });
  return {
    raw: [...catalog.raw],
    models: catalog.models.map(toPiModel),
  };
}

export type { AciServerModel };
export type AnyModel = Model<Api>;
