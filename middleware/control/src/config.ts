import { readFileSync } from 'node:fs';

/** Request/response format that shapes the upstream call (mirrors the contract). */
export type Format = 'openai' | 'anthropic';

/** Per-token prices; string|number|null, used by the gateway to price usage. */
export interface PricingConfig {
  inputCostPerToken?: string | number | null;
  outputCostPerToken?: string | number | null;
  cacheReadCostPerToken?: string | number | null;
  cacheCreationCostPerToken?: string | number | null;
}

/** One ordered failover candidate: `<provider>:<model>` + the upstream format. */
export interface RouteCandidate {
  routeId: string;
  format: Format;
  /** Self-hosted serving engine (sglang/vllm); absent for managed APIs. */
  engine?: 'sglang' | 'vllm';
}

export interface ModelEntry {
  pricing?: PricingConfig | null;
  candidates: RouteCandidate[];
}

export interface ControlConfig {
  /** Allow-listed sha256(api key) hex. Empty/absent => anonymous requests allowed. */
  keys?: string[];
  /** Public model id -> pricing + ordered route candidates. */
  models: Record<string, ModelEntry>;
}

const DEFAULT_PATH = '/etc/pag/control.config.json';

export function loadConfig(): ControlConfig {
  const path = process.env.CONTROL_CONFIG_PATH?.trim() || DEFAULT_PATH;
  const cfg = JSON.parse(readFileSync(path, 'utf8')) as ControlConfig;
  if (!cfg.models || typeof cfg.models !== 'object') {
    throw new Error(`control config ${path}: a "models" object is required`);
  }
  return cfg;
}
