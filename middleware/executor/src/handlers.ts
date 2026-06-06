import { Context } from 'hono';

import { forwardToBackend } from './backendForward';
import ProviderConfigs from './providers';
import { endpointStrings } from './providers/types';
import { resolveCandidates, RouteCandidate, Wire } from './routing';
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

/**
 * Build the `{ targets, body }` payload for the backend: shape the request for
 * every candidate (downstream format x candidate wire), then package it — one
 * shared body when all candidates share a wire, otherwise the envelope the
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
    body: transformToProviderRequest(candidate.wire, params, fn, {
      provider: candidate.wire,
    }),
  }));
  const sameWire = candidates.every((c) => c.wire === candidates[0].wire);
  return sameWire
    ? { targets, body: JSON.stringify(shaped[0].body) }
    : { targets, body: JSON.stringify({ candidates: shaped }) };
}

function responseTransformerFor(
  wire: Wire,
  fn: endpointStrings,
  streaming: boolean
): Function | undefined {
  const transforms = ProviderConfigs[wire]?.responseTransforms;
  if (!transforms) return undefined;
  return streaming ? transforms[`stream-${fn}`] : transforms[fn];
}

/**
 * Convert the backend response back to the downstream format. The committed
 * candidate's wire (from the backend's selected-route attribution header)
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
      selected.wire,
      responseTransformerFor(selected.wire, fn, true),
      requestURL,
      STRICT_OPENAI_COMPLIANCE,
      params
    );
  }
  return handleNonStreamingMode(
    backendResp,
    responseTransformerFor(selected.wire, fn, false),
    STRICT_OPENAI_COMPLIANCE,
    requestURL,
    params
  );
}

async function handle(c: Context, fn: endpointStrings): Promise<Response> {
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

  const candidates = resolveCandidates(params.model);
  if (candidates.length === 0) {
    return jsonError(
      400,
      'model_not_found',
      `no route configured for model ${params.model ?? '(none)'}`
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
  return driveResponse(backendResp, params, fn, candidates);
}

export const chatCompletions = (c: Context) => handle(c, 'chatComplete');
export const completions = (c: Context) => handle(c, 'complete');
export const embeddings = (c: Context) => handle(c, 'embed');
export const messages = (c: Context) => handle(c, 'messages');
