import Decimal from 'decimal.js';

// Canonical-ish usage shape: covers OpenAI (prompt_tokens/completion_tokens with
// prompt_tokens_details.cached_tokens as a subset) and Anthropic-style fields
// (input_tokens/output_tokens with cache_read_input_tokens and
// cache_creation_input_tokens as separate, non-overlapping buckets).
//
// Provider transforms (src/providers/*/chatComplete.ts) normalize Anthropic's
// `input_tokens` into a total `prompt_tokens` that already includes cached
// portions — so subtracting cache_read + cache_creation from prompt_tokens
// yields the non-cached input regardless of upstream provider family.
export interface Usage {
  prompt_tokens?: number;
  completion_tokens?: number;
  input_tokens?: number;
  output_tokens?: number;
  cache_read_input_tokens?: number;
  cache_creation_input_tokens?: number;
  prompt_tokens_details?: {
    cached_tokens?: number;
  };
}

export type PricePerToken = string | number | null | undefined;

export interface PricingConfig {
  inputCostPerToken: PricePerToken;
  outputCostPerToken: PricePerToken;
  cacheReadCostPerToken?: PricePerToken;
  cacheCreationCostPerToken?: PricePerToken;
}

export const isPriced = (v: PricePerToken): v is string | number =>
  v != null && v !== '';

const toDecimal = (v: PricePerToken): Decimal =>
  isPriced(v) ? new Decimal(v) : new Decimal(0);

export interface ResolvedUsage {
  promptTokens: number;
  completionTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
}

export const resolveUsage = (usage: Usage): ResolvedUsage => ({
  promptTokens: usage.prompt_tokens ?? usage.input_tokens ?? 0,
  completionTokens: usage.completion_tokens ?? usage.output_tokens ?? 0,
  cacheReadTokens:
    usage.cache_read_input_tokens ??
    usage.prompt_tokens_details?.cached_tokens ??
    0,
  cacheCreationTokens: usage.cache_creation_input_tokens ?? 0,
});

// Bills each bucket at its per-token rate. Unset cache rates fall back to
// inputCostPerToken — equivalent to "no cache discount" and identical to what
// the materialized total_cost in spend_logs computes from the same inputs.
export const computeCost = (usage: Usage, pricing: PricingConfig): number => {
  const {
    promptTokens,
    completionTokens,
    cacheReadTokens,
    cacheCreationTokens,
  } = resolveUsage(usage);

  const inputRate = toDecimal(pricing.inputCostPerToken);
  const cacheReadRate = isPriced(pricing.cacheReadCostPerToken)
    ? new Decimal(pricing.cacheReadCostPerToken)
    : inputRate;
  const cacheCreationRate = isPriced(pricing.cacheCreationCostPerToken)
    ? new Decimal(pricing.cacheCreationCostPerToken)
    : inputRate;
  const uncachedInput = Math.max(
    0,
    promptTokens - cacheReadTokens - cacheCreationTokens
  );

  return new Decimal(uncachedInput)
    .mul(inputRate)
    .add(new Decimal(cacheReadTokens).mul(cacheReadRate))
    .add(new Decimal(cacheCreationTokens).mul(cacheCreationRate))
    .add(
      new Decimal(completionTokens).mul(toDecimal(pricing.outputCostPerToken))
    )
    .toNumber();
};
