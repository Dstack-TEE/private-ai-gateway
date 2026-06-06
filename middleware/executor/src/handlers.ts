import { Context } from 'hono';

import { forwardToBackend } from './backendForward';
import {
  consultPost,
  consultPre,
  fetchCatalog,
  Format,
  hashApiKey,
  RouteCandidate,
} from './controlConsult';
import { meterResponse } from './cost';
import ProviderConfigs from './providers';
import { endpointStrings } from './providers/types';
import transformToProviderRequest from './services/transformToProviderRequest';
import { handleNonStreamingMode, handleStreamingMode } from './stream';
import { Params } from './types/requestBody';

// The prior gateway defaults to strict OpenAI compliance unless a client opts
// out; this executor doesn't expose that opt-out, so it's always strict.
const STRICT_OPENAI_COMPLIANCE = true;

function jsonError(status: number, type: string, message: string): Response {
  return new Response(JSON.stringify({ error: { type, message } }), {
    status,
    headers: { 'content-type': 'application/json' },
  });
}

/** A 429 response carrying the standard rate-limit headers. */
function rateLimitResponse(
  message: string,
  limit: number,
  resetAt: number
): Response {
  const retryAfter = Math.max(1, resetAt - Math.floor(Date.now() / 1000));
  return new Response(
    JSON.stringify({
      error: { message, type: 'rate_limit_error', code: 'rate_limit_exceeded' },
    }),
    {
      status: 429,
      headers: {
        'content-type': 'application/json',
        'X-RateLimit-Limit': String(limit),
        'X-RateLimit-Remaining': '0',
        'X-RateLimit-Reset': String(resetAt),
        'Retry-After': String(retryAfter),
      },
    }
  );
}

/**
 * Error type for a denial, using values valid on both surfaces this executor
 * serves (OpenAI and Anthropic share these four). Clients branch on the type
 * to decide retry behavior, so it's worth being precise rather than generic.
 */
function denialType(status: number): string {
  switch (status) {
    case 401:
      return 'authentication_error';
    case 402:
      return 'insufficient_quota';
    case 403:
      return 'permission_error';
    case 429:
      return 'rate_limit_error';
    default:
      return status >= 500 ? 'server_error' : 'invalid_request_error';
  }
}

/**
 * Build the `{ targets, body }` payload for the backend: shape the request for
 * every candidate (downstream format x candidate format), then package it — one
 * shared body when all candidates share a format, otherwise the envelope the
 * backend understands (`{ candidates: [{ target, body }] }`).
 */
function buildForwardPayload(
  params: Params,
  fn: endpointStrings,
  candidates: RouteCandidate[]
): { targets: string[]; body: string } {
  const targets = candidates.map((candidate) => candidate.routeId);
  const shaped = candidates.map((candidate) => ({
    target: candidate.routeId,
    body: transformToProviderRequest(candidate.format, params, fn, {
      provider: candidate.format,
    }),
  }));
  const sameFormat = candidates.every((c) => c.format === candidates[0].format);
  return sameFormat
    ? { targets, body: JSON.stringify(shaped[0].body) }
    : { targets, body: JSON.stringify({ candidates: shaped }) };
}

function responseTransformerFor(
  format: Format,
  fn: endpointStrings,
  streaming: boolean
): Function | undefined {
  const transforms = ProviderConfigs[format]?.responseTransforms;
  if (!transforms) return undefined;
  return streaming ? transforms[`stream-${fn}`] : transforms[fn];
}

/**
 * Convert the backend response back to the downstream format. The committed
 * candidate's format (from the backend's selected-route attribution header)
 * picks the response transform; buffered vs streaming is decided by the
 * backend response content-type, not the request flag (so an error returned
 * for a stream request is handled as buffered).
 */
function driveResponse(
  backendResp: Response,
  params: Params,
  fn: endpointStrings,
  candidates: RouteCandidate[]
): Response | Promise<Response> {
  const selectedRoute = backendResp.headers.get(
    'x-private-ai-gateway-selected-route'
  );
  const selected =
    candidates.find((c) => c.routeId === selectedRoute) ?? candidates[0];
  const streaming =
    backendResp.headers.get('content-type')?.includes('text/event-stream') ??
    false;
  // Only the `/complete` suffix matters here (it selects the SSE split pattern).
  const requestURL = `/${fn}`;

  if (streaming) {
    return handleStreamingMode(
      backendResp,
      selected.format,
      responseTransformerFor(selected.format, fn, true),
      requestURL,
      STRICT_OPENAI_COMPLIANCE,
      params
    );
  }
  return handleNonStreamingMode(
    backendResp,
    responseTransformerFor(selected.format, fn, false),
    STRICT_OPENAI_COMPLIANCE,
    requestURL,
    params
  );
}

async function handle(c: Context, fn: endpointStrings): Promise<Response> {
  const start = Date.now();
  const requestId = c.req.header('x-private-ai-gateway-request-id');
  if (!requestId) {
    return jsonError(
      400,
      'invalid_request_error',
      'missing x-private-ai-gateway-request-id'
    );
  }

  let params: Params;
  try {
    params = (await c.req.json()) as Params;
  } catch {
    return jsonError(400, 'invalid_request_error', 'invalid json body');
  }

  // Content-blind pre-request consult: authorization + pricing. Only the
  // bearer key's hash crosses the seam, never the raw key. A denial (invalid
  // key, exhausted quota, control unavailable) returns here before any
  // forwarding, so no inference happens and no receipt is emitted.
  const bearer = c.req.header('authorization')?.replace(/^Bearer\s+/i, '');
  const consult = await consultPre(
    params.model,
    bearer ? hashApiKey(bearer) : undefined
  );
  if (!consult.allow) {
    const status = consult.status ?? 403;
    const message = consult.message ?? 'forbidden';
    if (status === 429 && consult.rateLimit) {
      return rateLimitResponse(message, consult.rateLimit.limit, consult.rateLimit.resetAt);
    }
    return jsonError(status, denialType(status), message);
  }
  const pricing = consult.pricing ?? null;

  // The control plane ranks the model's deployments into ordered failover
  // candidates; an empty list means the model has no active deployment.
  const candidates = consult.candidates ?? [];
  if (candidates.length === 0) {
    return jsonError(
      400,
      'model_not_found',
      `no route available for model ${params.model ?? '(none)'}`
    );
  }

  const { targets, body } = buildForwardPayload(params, fn, candidates);
  let backendResp: Response;
  try {
    backendResp = await forwardToBackend({ requestId, targets, body });
  } catch (err) {
    return jsonError(
      502,
      'backend_unreachable',
      `failed to reach gateway backend: ${(err as Error).message}`
    );
  }
  const response = await driveResponse(backendResp, params, fn, candidates);

  // Content-blind post-request consult (billing + request log). Fired once the
  // raw upstream usage is known — immediately for buffered responses, at stream
  // end for SSE — with the route the backend committed to.
  const selectedRouteId = backendResp.headers.get(
    'x-private-ai-gateway-selected-route'
  );
  const attemptIndex = Math.max(
    0,
    candidates.findIndex((c) => c.routeId === selectedRouteId)
  );
  const isStreaming =
    backendResp.headers.get('content-type')?.includes('text/event-stream') ??
    false;
  return meterResponse(response, pricing, (usage) => {
    consultPost({
      requestId,
      endpoint: new URL(c.req.url).pathname,
      status: backendResp.status,
      durationMs: Date.now() - start,
      isStreaming,
      attemptIndex,
      selectedRouteId,
      requestModel: params.model ?? '',
      usage,
      pricing,
      spendMode: consult.spendMode,
      userId: consult.userId,
      virtualKeyId: consult.virtualKeyId,
    });
  });
}

export const chatCompletions = (c: Context) => handle(c, 'chatComplete');
export const completions = (c: Context) => handle(c, 'complete');
export const embeddings = (c: Context) => handle(c, 'embed');
export const messages = (c: Context) => handle(c, 'messages');

/** GET /v1/models — relay the catalog from the control plane. */
export const models = async (): Promise<Response> => {
  const r = await fetchCatalog();
  return new Response(r.body, {
    status: r.status,
    headers: { 'content-type': 'application/json' },
  });
};
