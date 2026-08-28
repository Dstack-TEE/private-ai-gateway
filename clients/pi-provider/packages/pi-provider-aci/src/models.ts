import type { Api, Model } from "@earendil-works/pi-ai";
import {
  discoverAciModelCatalog,
  mapAciModel,
  type AciModel,
  type AciServerModel,
} from "@phala/aci-provider";

import { toAciProviderConfig, type AciCloudConfig } from "./config.ts";

export type AciPiModel = Omit<Model<"openai-completions">, "api" | "provider" | "baseUrl">;

function piInput(model: AciModel): Array<"text" | "image"> {
  const input = model.input.filter(
    (modality): modality is "text" | "image" => modality === "text" || modality === "image",
  );
  if (input.length === 0) {
    throw new Error(`Pi does not support the input modalities declared by model "${model.id}"`);
  }
  return input;
}

function piCost(model: AciModel): AciPiModel["cost"] {
  return {
    input: model.cost.input,
    output: model.cost.output,
    cacheRead: model.cost.cacheRead ?? model.cost.input,
    cacheWrite: model.cost.cacheWrite ?? model.cost.input,
  };
}

export function mapAciModelToPi(model: AciModel): AciPiModel {
  return {
    id: model.id,
    name: model.name,
    reasoning: model.reasoning,
    input: piInput(model),
    cost: piCost(model),
    contextWindow: model.contextWindow,
    maxTokens: model.maxOutputTokens,
    compat: {
      thinkingFormat: "openrouter",
      maxTokensField: "max_tokens",
      supportsStore: true,
      supportsDeveloperRole: true,
      supportsStrictMode: false,
      supportsUsageInStreaming: true,
      supportsLongCacheRetention: false,
    },
  };
}

export function mapAciServerModel(
  model: AciServerModel,
  config: AciCloudConfig,
): AciPiModel | null {
  const mapped = mapAciModel(model, toAciProviderConfig(config));
  return mapped ? mapAciModelToPi(mapped) : null;
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
    models: catalog.models.map(mapAciModelToPi),
  };
}

export type { AciServerModel };
export type AnyModel = Model<Api>;
