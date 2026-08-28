import type { AciProviderConfig } from "./config.ts";
import type { AciFetch } from "@phala/aci-verifier/runtime";

export type AciModality = "text" | "audio" | "image" | "video" | "pdf";

const ACI_MODALITIES: ReadonlySet<unknown> = new Set(["text", "audio", "image", "video", "pdf"]);

export interface AciModel {
  id: string;
  name: string;
  description?: string;
  reasoning: boolean;
  toolCall: boolean;
  temperature: boolean;
  input: readonly AciModality[];
  output: readonly AciModality[];
  cost: {
    input: number;
    output: number;
    cacheRead?: number;
    cacheWrite?: number;
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
  supported_features?: unknown;
  supported_parameters?: unknown;
  supported_sampling_parameters?: unknown;
  description?: unknown;
}

export class AciModelDiscoveryError extends Error {
  constructor(message: string, options?: { cause?: unknown }) {
    super(message, options);
    this.name = "AciModelDiscoveryError";
  }
}

function invalidModel(id: string, field: string, expected: string): never {
  throw new AciModelDiscoveryError(`model "${id}" ${field} must be ${expected}`);
}

function nonEmptyString(value: unknown, id: string, field: string): string {
  if (typeof value !== "string" || value.length === 0) {
    return invalidModel(id, field, "a non-empty string");
  }
  return value;
}

function price(value: unknown, id: string, field: string): number {
  if (typeof value !== "string" && typeof value !== "number") {
    return invalidModel(id, field, "a non-negative number");
  }
  const parsed = Number(value);
  if (
    (typeof value === "string" && value.trim() === "") ||
    !Number.isFinite(parsed) ||
    parsed < 0
  ) {
    return invalidModel(id, field, "a non-negative number");
  }
  return Math.round(parsed * 1_000_000 * 1_000_000_000) / 1_000_000_000;
}

function modalities(value: unknown, id: string, field: string): readonly AciModality[] {
  if (!Array.isArray(value) || value.length === 0) {
    return invalidModel(id, field, "a non-empty array of supported modalities");
  }
  return value.map((item) => {
    if (!ACI_MODALITIES.has(item)) {
      return invalidModel(id, field, "an array of supported modalities");
    }
    return item as AciModality;
  });
}

function positiveInteger(value: unknown, id: string, field: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0) {
    return invalidModel(id, field, "a positive safe integer");
  }
  return value;
}

function stringSet(value: unknown, id: string, field: string): ReadonlySet<string> {
  if (!Array.isArray(value)) return invalidModel(id, field, "an array of strings");
  return new Set(value.map((item) => nonEmptyString(item, id, field)));
}

function pricing(value: unknown, id: string): AciModel["cost"] {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return invalidModel(id, "pricing", "an object");
  }
  const raw = value as Record<string, unknown>;
  return {
    input: price(raw.prompt, id, "pricing.prompt"),
    output: price(raw.completion, id, "pricing.completion"),
    ...(raw.input_cache_read === undefined
      ? {}
      : { cacheRead: price(raw.input_cache_read, id, "pricing.input_cache_read") }),
    ...(raw.input_cache_write === undefined
      ? {}
      : { cacheWrite: price(raw.input_cache_write, id, "pricing.input_cache_write") }),
  };
}

export function mapAciModel(
  model: AciServerModel,
  config: AciProviderConfig,
): AciModel | undefined {
  const id = nonEmptyString(model.id, "<unknown>", "id");
  if (config.models.isTeeOnly && model.is_tee !== true) return undefined;
  if (config.models.allowlist?.length && !config.models.allowlist.includes(id)) return undefined;

  const contextWindow = positiveInteger(model.context_length, id, "context_length");
  const maxOutputTokens = positiveInteger(model.max_output_length, id, "max_output_length");
  const features = stringSet(model.supported_features, id, "supported_features");
  const sampling = stringSet(
    model.supported_sampling_parameters,
    id,
    "supported_sampling_parameters",
  );

  return {
    id,
    name: nonEmptyString(model.name, id, "name"),
    ...(model.description === undefined
      ? {}
      : { description: nonEmptyString(model.description, id, "description") }),
    reasoning: features.has("reasoning"),
    toolCall: features.has("tools"),
    temperature: sampling.has("temperature"),
    input: modalities(model.input_modalities, id, "input_modalities"),
    output: modalities(model.output_modalities, id, "output_modalities"),
    cost: pricing(model.pricing, id),
    contextWindow,
    maxOutputTokens,
  };
}

export interface DiscoverAciModelsOptions {
  config: AciProviderConfig;
  fetch: AciFetch;
  signal?: AbortSignal;
  timeoutMs?: number;
}

export interface AciModelCatalog {
  raw: readonly AciServerModel[];
  models: readonly AciModel[];
}

export async function discoverAciModelCatalog({
  config,
  fetch,
  signal,
  timeoutMs = 10_000,
}: DiscoverAciModelsOptions): Promise<AciModelCatalog> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const url = new URL(config.baseURL);
    url.pathname = `${url.pathname.replace(/\/$/, "")}/models`;
    const response = await fetch(url, {
      signal: signal ? AbortSignal.any([signal, controller.signal]) : controller.signal,
      headers: {
        Accept: "application/json",
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
    const raw = (value as { data: unknown[] }).data.map((model, index) => {
      if (!model || typeof model !== "object" || Array.isArray(model)) {
        throw new AciModelDiscoveryError(`model catalog entry ${index} must be an object`);
      }
      return model as AciServerModel;
    });
    const models = raw
      .map((model) => mapAciModel(model, config))
      .filter((model): model is AciModel => model !== undefined);
    const ids = new Set<string>();
    for (const model of models) {
      if (ids.has(model.id)) {
        throw new AciModelDiscoveryError(`model catalog contains duplicate id "${model.id}"`);
      }
      ids.add(model.id);
    }
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
