// Model discovery and mapping. Pulls /v1/models from the ACI gateway and
// converts each entry into opencode's provider model config shape. Responsible
// for:
//   - is_tee filtering (config.models.isTeeOnly)
//   - allowlist filtering (config.models.allowlist)
//   - embedding model exclusion (output_modalities === ["embeddings"])
//   - reasoning inference (by model family id)
//   - pricing conversion (per-token -> per-million-token)
//
// Note: unlike the pi provider there is no thinkingFormat mapping here. Pi's
// built-in openai-completions handler sends `enable_thinking` /
// `reasoning_effort` based on compat settings; opencode's
// @ai-sdk/openai-compatible has no equivalent config surface, so reasoning is
// advertised on the model (`reasoning: true`) and reasoning_content streamed
// by the gateway still surfaces in opencode, but no thinking parameter is
// sent. This is a documented fidelity gap vs the pi provider.

import { type AciCloudConfig } from "./config.ts";
import { DEFAULT_DISCOVERY_TIMEOUT_MS, LOG_PREFIX, buildModelsUrl } from "./constants.ts";
import { profile } from "./profile.ts";

export interface AciServerModel {
  id?: unknown;
  name?: unknown;
  is_tee?: unknown;
  context_length?: unknown;
  max_output_length?: unknown;
  pricing?: unknown;
  providers?: unknown;
  input_modalities?: unknown;
  output_modalities?: unknown;
  supported_parameters?: unknown;
  description?: unknown;
}

/** opencode provider model config (config.provider.<id>.models.<modelId>). */
export interface OpencodeModelConfig {
  name: string;
  reasoning: boolean;
  cost: { input: number; output: number; cache_read: number; cache_write: number };
  limit: { context: number; output: number };
  modalities: { input: ("text" | "image")[]; output: "text"[] };
}

export type OpencodeModelsRecord = Record<string, OpencodeModelConfig>;

// Pure inference. Exposed for tests so the model-family mapping can be
// verified without hitting the network.
export function inferReasoning(modelId: string): boolean {
  const id = modelId.toLowerCase();
  if (id.includes("qwen")) return true;
  if (id.includes("gpt-oss")) return true;
  if ((id.includes("deepseek") && id.includes("r1")) || id.includes("reasoner")) return true;
  return false;
}

function parsePerTokenPrice(value: unknown): number {
  if (typeof value !== "string" && typeof value !== "number") return 0;
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed < 0) return 0;
  // ACI returns per-token pricing; opencode expects per-million-token.
  return parsed * 1_000_000;
}

function mapInputModalities(raw: unknown): ("text" | "image")[] {
  if (!Array.isArray(raw)) return ["text"];
  const hasImage = raw.some((m) => m === "image");
  return hasImage ? ["text", "image"] : ["text"];
}

function isEmbeddingModel(model: AciServerModel): boolean {
  const output = model.output_modalities;
  return Array.isArray(output) && output.length === 1 && output[0] === "embeddings";
}

// Pure mapping. Exposed for tests.
export function mapAciServerModel(
  model: AciServerModel,
  config: AciCloudConfig,
): OpencodeModelConfig | null {
  if (typeof model.id !== "string" || model.id.length === 0) return null;
  if (isEmbeddingModel(model)) return null;

  if (config.models.isTeeOnly && model.is_tee !== true) return null;

  if (config.models.allowlist && config.models.allowlist.length > 0) {
    if (!config.models.allowlist.includes(model.id)) return null;
  }

  const contextWindow =
    typeof model.context_length === "number" && model.context_length > 0
      ? model.context_length
      : 32768;
  const maxTokens =
    typeof model.max_output_length === "number" && model.max_output_length > 0
      ? model.max_output_length
      : Math.min(contextWindow, 8192);

  const pricing =
    model.pricing && typeof model.pricing === "object"
      ? (model.pricing as { prompt?: unknown; completion?: unknown })
      : {};

  return {
    name: typeof model.name === "string" && model.name ? model.name : model.id,
    reasoning: inferReasoning(model.id),
    cost: {
      input: parsePerTokenPrice(pricing.prompt),
      output: parsePerTokenPrice(pricing.completion),
      cache_read: 0,
      cache_write: 0,
    },
    limit: { context: contextWindow, output: maxTokens },
    modalities: { input: mapInputModalities(model.input_modalities), output: ["text"] },
  };
}

export interface DiscoverAciModelsOptions {
  timeoutMs?: number;
  baseUrl?: string;
}

export interface DiscoverAciModelsResult {
  models: OpencodeModelsRecord;
  raw: AciServerModel[];
}

export async function discoverAciModels(
  apiKey: string,
  config: AciCloudConfig,
  options: DiscoverAciModelsOptions = {},
): Promise<DiscoverAciModelsResult> {
  if (!apiKey) return { models: {}, raw: [] };

  const timeoutMs = options.timeoutMs ?? DEFAULT_DISCOVERY_TIMEOUT_MS;
  const controller = new AbortController();
  const timeout =
    timeoutMs > 0 ? setTimeout(() => controller.abort(), timeoutMs).unref() : undefined;

  try {
    const response = await fetch(buildModelsUrl(options.baseUrl ?? config.baseUrl), {
      signal: controller.signal,
      headers: {
        Authorization: `Bearer ${apiKey}`,
        Accept: "application/json",
      },
    });
    if (!response.ok) {
      console.error(`${LOG_PREFIX} /v1/models returned ${response.status} ${response.statusText}`);
      return { models: {}, raw: [] };
    }
    const json = (await response.json()) as { data?: unknown };
    const list = Array.isArray(json.data)
      ? (json.data as AciServerModel[]).filter((m) => m && typeof m === "object")
      : [];
    const models: OpencodeModelsRecord = {};
    for (const raw of list) {
      if (typeof raw.id !== "string" || raw.id.length === 0) continue;
      const mapped = mapAciServerModel(raw, config);
      if (mapped) models[raw.id] = mapped;
    }
    return { models, raw: list };
  } catch (error) {
    console.error(`${LOG_PREFIX} model discovery failed:`, error);
    return { models: {}, raw: [] };
  } finally {
    if (timeout) clearTimeout(timeout);
  }
}

/** Fallback model record used when discovery has no API key or fails. Drawn
 *  from the active provider profile (branded shells supply their own
 *  catalog); the live /v1/models catalog is authoritative. */
export function fallbackModels(): OpencodeModelsRecord {
  const out: OpencodeModelsRecord = {};
  for (const m of profile().fallbackModels) {
    out[m.id] = {
      name: m.name,
      reasoning: m.reasoning,
      cost: {
        input: m.cost.input,
        output: m.cost.output,
        cache_read: m.cost.cacheRead,
        cache_write: m.cost.cacheWrite,
      },
      limit: { context: m.contextWindow, output: m.maxTokens },
      modalities: { input: m.input, output: ["text"] },
    };
  }
  return out;
}
