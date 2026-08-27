import type { AciProviderConfig, AciThinkingFormat } from "./config.ts";
import type { AciFetch } from "@phala/aci-verifier/runtime";

export type AciModality = "text" | "audio" | "image" | "video" | "pdf";

export interface AciModel {
  id: string;
  name: string;
  family?: string;
  description?: string;
  reasoning: boolean;
  thinkingFormat: Exclude<AciThinkingFormat, "auto">;
  toolCall: boolean;
  temperature: boolean;
  input: readonly AciModality[];
  output: readonly AciModality[];
  cost: {
    input: number;
    output: number;
    cacheRead: number;
    cacheWrite: number;
  };
  contextWindow: number;
  maxOutputTokens: number;
}

export interface AciServerModel {
  id?: unknown;
  name?: unknown;
  is_tee?: unknown;
  context_length?: unknown;
  max_output_length?: unknown;
  pricing?: unknown;
  input_modalities?: unknown;
  output_modalities?: unknown;
  supported_parameters?: unknown;
  description?: unknown;
}

export class AciModelDiscoveryError extends Error {
  constructor(message: string, options?: { cause?: unknown }) {
    super(message, options);
    this.name = "AciModelDiscoveryError";
  }
}

export function inferThinkingFormat(modelId: string): Exclude<AciThinkingFormat, "auto"> {
  const id = modelId.toLowerCase();
  if (id.includes("qwen")) return "qwen";
  if (
    id.includes("gpt-oss") ||
    (id.includes("deepseek") && (id.includes("r1") || id.includes("v4"))) ||
    id.includes("reasoner") ||
    id.includes("glm-5")
  ) {
    return "openai";
  }
  return "off";
}

function price(value: unknown): number {
  if (typeof value !== "string" && typeof value !== "number") return 0;
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed < 0) return 0;
  return Math.round(parsed * 1_000_000 * 1_000_000_000) / 1_000_000_000;
}

function modalities(value: unknown, fallback: readonly AciModality[]): readonly AciModality[] {
  if (!Array.isArray(value)) return fallback;
  const supported = new Set<AciModality>(["text", "audio", "image", "video", "pdf"]);
  const result = value.filter((item): item is AciModality => supported.has(item as AciModality));
  return result.length > 0 ? result : fallback;
}

function supportedParameters(value: unknown): Set<string> {
  return new Set(
    Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [],
  );
}

export function mapAciModel(
  model: AciServerModel,
  config: AciProviderConfig,
): AciModel | undefined {
  if (typeof model.id !== "string" || model.id.length === 0) return undefined;
  if (
    Array.isArray(model.output_modalities) &&
    model.output_modalities.length === 1 &&
    model.output_modalities[0] === "embeddings"
  ) {
    return undefined;
  }
  const output = modalities(model.output_modalities, ["text"]);
  if (config.models.isTeeOnly && model.is_tee !== true) return undefined;
  if (config.models.allowlist?.length && !config.models.allowlist.includes(model.id))
    return undefined;

  const contextWindow =
    typeof model.context_length === "number" && model.context_length > 0
      ? model.context_length
      : 32_768;
  const maxOutputTokens =
    typeof model.max_output_length === "number" && model.max_output_length > 0
      ? model.max_output_length
      : Math.min(contextWindow, 8_192);
  const rawPricing =
    model.pricing && typeof model.pricing === "object"
      ? (model.pricing as Record<string, unknown>)
      : {};
  const parameters = supportedParameters(model.supported_parameters);
  const thinkingFormat =
    config.models.thinkingFormat === "auto"
      ? inferThinkingFormat(model.id)
      : config.models.thinkingFormat;

  return {
    id: model.id,
    name: typeof model.name === "string" && model.name ? model.name : model.id,
    family: model.id.split("/").at(-1)?.split(/[-:]/)[0],
    ...(typeof model.description === "string" && model.description
      ? { description: model.description }
      : {}),
    reasoning: thinkingFormat !== "off",
    thinkingFormat,
    toolCall: parameters.size === 0 || parameters.has("tools") || parameters.has("tool_choice"),
    temperature: parameters.size === 0 || parameters.has("temperature"),
    input: modalities(model.input_modalities, ["text"]),
    output,
    cost: {
      input: price(rawPricing.prompt),
      output: price(rawPricing.completion),
      cacheRead: price(rawPricing.input_cache_read ?? rawPricing.cache_read),
      cacheWrite: price(rawPricing.input_cache_write ?? rawPricing.cache_write),
    },
    contextWindow,
    maxOutputTokens,
  };
}

export interface DiscoverAciModelsOptions {
  apiKey?: string;
  config: AciProviderConfig;
  fetch: AciFetch;
  timeoutMs?: number;
}

export interface AciModelCatalog {
  raw: readonly AciServerModel[];
  models: readonly AciModel[];
}

export async function discoverAciModelCatalog({
  apiKey,
  config,
  fetch,
  timeoutMs = 10_000,
}: DiscoverAciModelsOptions): Promise<AciModelCatalog> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const url = new URL(config.baseURL);
    url.pathname = `${url.pathname.replace(/\/$/, "")}/models`;
    const response = await fetch(url, {
      signal: controller.signal,
      headers: {
        Accept: "application/json",
        ...(apiKey ? { Authorization: `Bearer ${apiKey}` } : {}),
      },
    });
    if (!response.ok) {
      throw new AciModelDiscoveryError(
        `model discovery returned HTTP ${response.status} ${response.statusText}`,
      );
    }
    const value: unknown = await response.json();
    if (!value || typeof value !== "object" || !Array.isArray((value as { data?: unknown }).data)) {
      throw new AciModelDiscoveryError("model discovery returned an invalid catalog");
    }
    const raw = (value as { data: AciServerModel[] }).data.filter(
      (model) => model && typeof model === "object" && !Array.isArray(model),
    );
    const models = raw
      .map((model) => mapAciModel(model, config))
      .filter((model): model is AciModel => model !== undefined);
    return { raw, models };
  } catch (error) {
    if (error instanceof AciModelDiscoveryError) throw error;
    throw new AciModelDiscoveryError("model discovery failed", { cause: error });
  } finally {
    clearTimeout(timeout);
  }
}

export async function discoverAciModels(options: DiscoverAciModelsOptions): Promise<AciModel[]> {
  const catalog = await discoverAciModelCatalog(options);
  return [...catalog.models];
}
