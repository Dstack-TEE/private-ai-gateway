import type { Api, Model, ThinkingLevelMap } from "@earendil-works/pi-ai";
import {
  discoverAciModelCatalog,
  mapAciModel,
  type AciModel,
  type AciServerModel,
} from "@phala/aci-provider/models";

import { toAciProviderConfig, type AciCloudConfig } from "./config.ts";

export type AciPiModel = Omit<Model<"openai-completions">, "api" | "provider" | "baseUrl">;

/**
 * Per-model compatibility overrides applied on top of the shared catalog
 * mapping. Upstreams differ in which OpenAI-compatible surface they accept;
 * the catalog contract does not declare these, so known quirks are recorded
 * here and users can patch them via `models.overrides` in the provider
 * config. Every field falls back to the shared default when unset.
 */
export interface ModelCompatOverride {
  /** Rewrite the system prompt to a `system` (instead of `developer`) message. */
  supportsDeveloperRole?: boolean;
  /** Map pi thinking levels to this model's reasoning-effort vocabulary. */
  thinkingLevelMap?: ThinkingLevelMap;
  /** Cap the advertised max output tokens (e.g. catalog metadata exceeds the upstream limit). */
  maxTokens?: number;
}

/**
 * Known upstream quirks, keyed by catalog model id. These are properties of
 * the model served behind the gateway, not of a brand, so the vendor-neutral
 * core carries them; deployments can still override per model via config.
 */
export const BUILTIN_MODEL_COMPAT_OVERRIDES: Record<string, ModelCompatOverride> = {
  // These upstreams reject the `developer` message role (they accept `system`).
  "qwen/qwen3.8-27b": { supportsDeveloperRole: false },
  "qwen/qwen3.5-27b": { supportsDeveloperRole: false },
  "qwen/qwen3.5-397b-a17b": { supportsDeveloperRole: false },
  "phala/qwen3.8-27b-uncensored": {
    supportsDeveloperRole: false,
    // Accepted reasoning efforts are xhigh (default), medium and low; `high`
    // and `max` are remapped to xhigh, `minimal` to low, and `off` omits the
    // reasoning field entirely (this upstream rejects reasoning_effort "none").
    thinkingLevelMap: { off: null, minimal: "low", high: "xhigh", max: "xhigh" },
  },
  // These upstreams reject an explicit reasoning_effort of "none", and only
  // accept low/medium/high. pi clamps "off" to "minimal", so minimal maps to
  // low; xhigh/max map to high.
  "openai/gpt-oss-120b": {
    thinkingLevelMap: { off: null, minimal: "low", xhigh: "high", max: "high" },
  },
  "deepseek/deepseek-v3.2": {
    thinkingLevelMap: { off: null, minimal: "low", xhigh: "high", max: "high" },
  },
};

/** Effective override for a catalog model: config patch over the builtin table. */
export function resolveModelCompatOverride(
  config: Pick<AciCloudConfig, "models">,
  modelId: string,
): ModelCompatOverride | undefined {
  const builtin = BUILTIN_MODEL_COMPAT_OVERRIDES[modelId];
  const patch = config.models.overrides?.[modelId];
  if (!builtin) return patch;
  if (!patch) return builtin;
  return {
    ...builtin,
    ...patch,
    // Merge the thinking-level map key-by-key so a config patch can adjust a
    // single level without discarding the builtin mappings.
    ...(builtin.thinkingLevelMap || patch.thinkingLevelMap
      ? { thinkingLevelMap: { ...builtin.thinkingLevelMap, ...patch.thinkingLevelMap } }
      : {}),
  };
}

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

export function mapAciModelToPi(model: AciModel, compatOverride?: ModelCompatOverride): AciPiModel {
  return {
    id: model.id,
    name: model.name,
    reasoning: model.reasoning,
    input: piInput(model),
    cost: piCost(model),
    contextWindow: model.contextWindow,
    maxTokens: compatOverride?.maxTokens ?? model.maxOutputTokens,
    ...(compatOverride?.thinkingLevelMap
      ? { thinkingLevelMap: compatOverride.thinkingLevelMap }
      : {}),
    compat: {
      thinkingFormat: "openrouter",
      maxTokensField: "max_tokens",
      supportsStore: true,
      supportsDeveloperRole: compatOverride?.supportsDeveloperRole ?? true,
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
  if (!mapped || typeof model.id !== "string") return null;
  return mapAciModelToPi(mapped, resolveModelCompatOverride(config, model.id));
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
    models: catalog.models.map((m) => mapAciModelToPi(m, resolveModelCompatOverride(config, m.id))),
  };
}

export type { AciServerModel };
export type AnyModel = Model<Api>;
