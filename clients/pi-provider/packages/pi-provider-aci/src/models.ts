// Model discovery and mapping. Pulls /v1/models from the Aci gateway and
// converts each entry into a pi ProviderModelConfig. Responsible for:
//   - is_tee filtering (config.models.isTeeOnly)
//   - allowlist filtering (config.models.allowlist)
//   - embedding model exclusion (output_modalities === ["embeddings"])
//   - thinkingFormat inference (config.models.thinkingFormat, "auto" by id)
//   - pricing conversion (per-token -> per-million-token)
//   - compat settings so the built-in openai-completions handler sends the
//     right max-tokens field and thinking parameter for each model family
//
// Thinking works without a custom streamSimple because pi's built-in
// openai-completions handler:
//   - sends `enable_thinking: boolean` when compat.thinkingFormat === "qwen"
//     and model.reasoning === true
//   - sends `reasoning_effort` when thinkingFormat === "openai"
//   - parses streaming `reasoning_content` / `reasoning` / `reasoning_text`
//     deltas into pi thinking blocks regardless of the request format

import type { Api, Model } from "@earendil-works/pi-ai";
import type { ProviderModelConfig } from "@earendil-works/pi-coding-agent";

import { type AciCloudConfig } from "./config.ts";
import { DEFAULT_DISCOVERY_TIMEOUT_MS, LOG_PREFIX, buildModelsUrl } from "./constants.ts";
import { DEFAULT_PROFILE, type ProviderProfile } from "./profile.ts";

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

interface InferredThinking {
  reasoning: boolean;
  format: "qwen" | "openai" | "off";
  maxTokensField: "max_tokens" | "max_completion_tokens";
  supportsReasoningEffort: boolean;
}

// Pure inference. Exposed for tests so the model-family mapping can be
// verified without hitting the network.
export function inferThinkingFormat(modelId: string): InferredThinking {
  const id = modelId.toLowerCase();

  if (id.includes("qwen")) {
    // Qwen3 accepts top-level enable_thinking: boolean and streams
    // reasoning_content. This is the primary thinking path for Aci.
    return {
      reasoning: true,
      format: "qwen",
      maxTokensField: "max_tokens",
      supportsReasoningEffort: false,
    };
  }

  if (id.includes("gpt-oss")) {
    // OpenAI reasoning models use reasoning_effort and max_completion_tokens.
    return {
      reasoning: true,
      format: "openai",
      maxTokensField: "max_completion_tokens",
      supportsReasoningEffort: true,
    };
  }

  if ((id.includes("deepseek") && id.includes("r1")) || id.includes("reasoner")) {
    // Conservative: treat as OpenAI-style reasoning. If the upstream rejects
    // reasoning_effort, the user can set thinkingFormat: "off".
    return {
      reasoning: true,
      format: "openai",
      maxTokensField: "max_tokens",
      supportsReasoningEffort: true,
    };
  }

  // Non-reasoning models. The handler still surfaces any reasoning_content the
  // model happens to stream, but no thinking parameter is sent.
  return {
    reasoning: false,
    format: "off",
    maxTokensField: "max_tokens",
    supportsReasoningEffort: false,
  };
}

function resolveThinking(modelId: string, config: AciCloudConfig): InferredThinking {
  const configured = config.models.thinkingFormat;
  if (configured === "off") {
    return {
      reasoning: false,
      format: "off",
      maxTokensField: "max_tokens",
      supportsReasoningEffort: false,
    };
  }
  if (configured === "qwen") {
    return {
      reasoning: true,
      format: "qwen",
      maxTokensField: "max_tokens",
      supportsReasoningEffort: false,
    };
  }
  if (configured === "openai") {
    return {
      reasoning: true,
      format: "openai",
      maxTokensField: "max_completion_tokens",
      supportsReasoningEffort: true,
    };
  }
  return inferThinkingFormat(modelId);
}

function parsePerTokenPrice(value: unknown): number {
  if (typeof value !== "string" && typeof value !== "number") return 0;
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed < 0) return 0;
  // Aci returns per-token pricing; pi expects per-million-token.
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
): ProviderModelConfig | null {
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

  const thinking = resolveThinking(model.id, config);

  const compat: NonNullable<ProviderModelConfig["compat"]> = {
    thinkingFormat: thinking.format === "off" ? "openai" : thinking.format,
    maxTokensField: thinking.maxTokensField,
    supportsReasoningEffort: thinking.supportsReasoningEffort,
    supportsStrictMode: false,
    supportsUsageInStreaming: true,
  };

  return {
    id: model.id,
    name: typeof model.name === "string" && model.name ? model.name : model.id,
    reasoning: thinking.reasoning,
    input: mapInputModalities(model.input_modalities),
    cost: {
      input: parsePerTokenPrice(pricing.prompt),
      output: parsePerTokenPrice(pricing.completion),
      cacheRead: 0,
      cacheWrite: 0,
    },
    contextWindow,
    maxTokens,
    compat,
  };
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
  if (!apiKey) return { models: [], raw: [] };

  const timeoutMs = options.timeoutMs ?? DEFAULT_DISCOVERY_TIMEOUT_MS;
  const controller = new AbortController();
  const timeout =
    timeoutMs > 0 ? setTimeout(() => controller.abort(), timeoutMs).unref() : undefined;

  try {
    const response = await (options.fetch ?? globalThis.fetch)(buildModelsUrl(options.baseUrl), {
      signal: controller.signal,
      headers: {
        Authorization: `Bearer ${apiKey}`,
        Accept: "application/json",
      },
    });
    if (!response.ok) {
      console.error(
        `${options.logPrefix ?? LOG_PREFIX} /v1/models returned ${response.status} ${response.statusText}`,
      );
      return { models: [], raw: [] };
    }
    const json = (await response.json()) as { data?: unknown };
    const list = Array.isArray(json.data)
      ? (json.data as AciServerModel[]).filter((m) => m && typeof m === "object")
      : [];
    const models = list
      .map((m) => mapAciServerModel(m, config))
      .filter((m): m is ProviderModelConfig => m !== null);
    return { models, raw: list };
  } catch (error) {
    console.error(`${options.logPrefix ?? LOG_PREFIX} model discovery failed:`, error);
    return { models: [], raw: [] };
  } finally {
    if (timeout) clearTimeout(timeout);
  }
}

/** Fallback model list used when discovery has no API key or fails. Branded
 *  shells supply their own catalog; the live /v1/models catalog is authoritative. */
export function fallbackModels(
  providerProfile: ProviderProfile = DEFAULT_PROFILE,
): ProviderModelConfig[] {
  return providerProfile.fallbackModels.map((m) => {
    const thinking = inferThinkingFormat(m.id);
    return {
      ...m,
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

// Re-exported type alias so index.ts can name the Model<Api> shape without an
// extra import site.
export type AnyModel = Model<Api>;
