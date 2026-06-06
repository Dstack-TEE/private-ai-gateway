import { readFileSync } from 'node:fs';

export type Wire = 'openai' | 'anthropic';

/** One ordered failover candidate: a backend route id and the upstream wire format. */
export interface RouteCandidate {
  /** `<upstream name>:<public model id>`, aligned with the backend's upstreams.json. */
  routeId: string;
  /** Which provider transform set to shape the request with / parse the response with. */
  wire: Wire;
}

/**
 * Routing stub: a static `model -> ordered candidates` map loaded from a JSON
 * file (PRIVATE_AI_GATEWAY_EXECUTOR_ROUTES_PATH). Intended to be replaced by a
 * dynamic ranking source over the control IPC, keeping the same output shape
 * (an ordered list of {routeId, wire}).
 */
type RoutesConfig = Record<string, RouteCandidate[]>;

let cache: RoutesConfig | null = null;

function loadRoutes(): RoutesConfig {
  if (cache) return cache;
  const path = process.env.PRIVATE_AI_GATEWAY_EXECUTOR_ROUTES_PATH?.trim();
  cache = path ? (JSON.parse(readFileSync(path, 'utf8')) as RoutesConfig) : {};
  return cache;
}

/** Resolve the ordered failover candidates for a public model id (empty if unknown). */
export function resolveCandidates(model: string | undefined): RouteCandidate[] {
  if (!model) return [];
  return loadRoutes()[model] ?? [];
}
