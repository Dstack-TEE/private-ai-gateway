import { Context } from "hono";

import {
  errorResponse,
  errorType,
  normalizeUpstreamError,
  rateLimitResponse,
  Surface,
} from "./errors/response";
import { forwardToBackend } from "./integrations/backend";
import {
  consultPost,
  consultPre,
  fetchCatalog,
  Format,
  hashApiKey,
  RouteCandidate,
} from "./integrations/control";
import { meterResponse } from "./cost";
import ProviderConfigs from "./providers";
import { endpointStrings } from "./providers/types";
import transformToProviderRequest from "./services/transformToProviderRequest";
import { handleNonStreamingMode, handleStreamingMode } from "./stream";
import { Params } from "./types/requestBody";

// This executor does not expose a strict-compliance opt-out, so it is always
// strict.
const STRICT_OPENAI_COMPLIANCE = true;

// The downstream client surface is fixed by the endpoint: /v1/messages speaks
// Anthropic, everything else speaks OpenAI. This drives the error envelope so
// errors match what success responses already do for each surface.
const surfaceFor = (fn: endpointStrings): Surface =>
  fn === "messages" ? "anthropic" : "openai";

/**
 * Build the `{ targets, body }` payload for the backend: shape the request for
 * every candidate (by its format + engine), then package it — one shared body
 * when all candidates shape identically, otherwise the envelope the backend
 * understands (`{ candidates: [{ target, body }] }`).
 */
function buildForwardPayload(
  params: Params,
  fn: endpointStrings,
  candidates: RouteCandidate[],
): { targets: string[]; body: string } {
  const targets = candidates.map((candidate) => candidate.routeId);
  const shaped = candidates.map((candidate) => ({
    target: candidate.routeId,
    body: transformToProviderRequest(candidate.format, params, fn, {
      provider: candidate.format,
      engine: candidate.engine,
    }),
  }));
  // A shared body is only valid when every candidate shapes the request
  // identically — same format AND same engine (engine alters the openai shaping).
  const sameShape = candidates.every(
    (c) =>
      c.format === candidates[0].format && c.engine === candidates[0].engine,
  );
  return sameShape
    ? { targets, body: JSON.stringify(shaped[0].body) }
    : { targets, body: JSON.stringify({ candidates: shaped }) };
}

function responseTransformerFor(
  format: Format,
  fn: endpointStrings,
  streaming: boolean,
): Function | undefined {
  const transforms = ProviderConfigs[format]?.responseTransforms;
  if (!transforms) return undefined;
  return streaming ? transforms[`stream-${fn}`] : transforms[fn];
}

/**
 * Convert a successful backend response back to the downstream format. Non-2xx
 * responses are sanitized and returned before reaching this transform. The
 * committed candidate's format (from the backend's selected-route attribution
 * header) picks the response transform; buffered vs streaming is decided by the
 * backend response content-type, not the request flag.
 */
function driveResponse(
  backendResp: Response,
  params: Params,
  fn: endpointStrings,
  candidates: RouteCandidate[],
  surface: Surface,
  requestId: string,
): Response | Promise<Response> {
  // Non-2xx upstream responses are sanitized into a consistent, surface-shaped
  // error before reaching the client (upstream status/body/headers are never
  // relayed verbatim). Short-circuit here so the error body never gets run
  // through the success-shaped response transform.
  if (!backendResp.ok) {
    return normalizeUpstreamError(backendResp, surface, requestId);
  }

  const selectedRoute = backendResp.headers.get(
    "x-private-ai-gateway-selected-route",
  );
  const selected =
    candidates.find((c) => c.routeId === selectedRoute) ?? candidates[0];
  const streaming =
    backendResp.headers.get("content-type")?.includes("text/event-stream") ??
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
      params,
    );
  }
  return handleNonStreamingMode(
    backendResp,
    responseTransformerFor(selected.format, fn, false),
    STRICT_OPENAI_COMPLIANCE,
    requestURL,
    params,
  );
}

async function handle(c: Context, fn: endpointStrings): Promise<Response> {
  const start = Date.now();
  const surface = surfaceFor(fn);
  const requestId = c.req.header("x-private-ai-gateway-request-id");
  if (!requestId) {
    return errorResponse(
      surface,
      400,
      "invalid_request_error",
      "missing x-private-ai-gateway-request-id",
    );
  }

  let params: Params;
  try {
    params = (await c.req.json()) as Params;
  } catch {
    return errorResponse(
      surface,
      400,
      "invalid_request_error",
      "invalid json body",
      requestId,
    );
  }

  // Pre-request consult: authorization + pricing. Only the
  // bearer key's hash crosses the seam, never the raw key. A denial (invalid
  // key, exhausted quota, control unavailable) returns here before any
  // forwarding, so no inference happens and no receipt is emitted.
  const bearer = c.req.header("authorization")?.replace(/^Bearer\s+/i, "");
  const consult = await consultPre(
    params.model,
    bearer ? hashApiKey(bearer) : undefined,
    params.provider,
  );
  if (!consult.allow) {
    const status = consult.status ?? 403;
    const message = consult.message ?? "forbidden";
    if (status === 429 && consult.rateLimit) {
      return rateLimitResponse(
        surface,
        message,
        consult.rateLimit.limit,
        consult.rateLimit.resetAt,
        requestId,
      );
    }
    return errorResponse(
      surface,
      status,
      errorType(surface, status),
      message,
      requestId,
    );
  }
  const pricing = consult.pricing ?? null;

  // The control plane ranks the model's deployments into ordered failover
  // candidates; an empty list means the model has no active deployment.
  const candidates = consult.candidates ?? [];
  if (candidates.length === 0) {
    return errorResponse(
      surface,
      400,
      "model_not_found",
      `no route available for model ${params.model ?? "(none)"}`,
      requestId,
    );
  }

  const { targets, body } = buildForwardPayload(params, fn, candidates);
  let backendResp: Response;
  try {
    backendResp = await forwardToBackend({
      requestId,
      targets,
      body,
      userTier: consult.userTier,
      signal: c.req.raw.signal,
    });
  } catch (err) {
    return errorResponse(
      surface,
      502,
      "backend_unreachable",
      `failed to reach gateway backend: ${(err as Error).message}`,
      requestId,
    );
  }
  const response = await driveResponse(
    backendResp,
    params,
    fn,
    candidates,
    surface,
    requestId,
  );

  // Post-request consult (billing + request log). Fired once the
  // raw upstream usage is known — immediately for buffered responses, at stream
  // end for SSE — with the route the backend committed to.
  const selectedRouteId = backendResp.headers.get(
    "x-private-ai-gateway-selected-route",
  );
  const attemptIndex = Math.max(
    0,
    candidates.findIndex((c) => c.routeId === selectedRouteId),
  );
  const isStreaming =
    backendResp.headers.get("content-type")?.includes("text/event-stream") ??
    false;

  // Report each failed-over attempt (header value is `route_id=status`,
  // comma-separated in the order tried) to the control plane as its own usage
  // report, carrying no usage since the attempt produced no response body.
  const failedAttempts = backendResp.headers.get(
    "x-private-ai-gateway-failed-attempts",
  );
  if (failedAttempts) {
    const endpoint = new URL(c.req.url).pathname;
    failedAttempts.split(",").forEach((entry, i) => {
      const eq = entry.lastIndexOf("=");
      if (eq <= 0) return;
      const failedStatus = Number(entry.slice(eq + 1));
      // `Number("")` is 0, not NaN — reject non-positive so a malformed
      // `route=` entry can't post a bogus status-0 row.
      if (!Number.isInteger(failedStatus) || failedStatus <= 0) return;
      consultPost({
        requestId,
        endpoint,
        status: failedStatus,
        durationMs: 0,
        isStreaming,
        attemptIndex: i,
        selectedRouteId: entry.slice(0, eq),
        requestModel: params.model ?? "",
        usage: null,
        pricing,
        spendMode: consult.spendMode,
        userId: consult.userId,
        virtualKeyId: consult.virtualKeyId,
      });
    });
  }

  return meterResponse(response, pricing, start, (usage, ttftMs) => {
    consultPost({
      requestId,
      endpoint: new URL(c.req.url).pathname,
      status: backendResp.status,
      durationMs: Date.now() - start,
      ttftMs,
      isStreaming,
      attemptIndex,
      selectedRouteId,
      requestModel: params.model ?? "",
      usage,
      pricing,
      spendMode: consult.spendMode,
      userId: consult.userId,
      virtualKeyId: consult.virtualKeyId,
    });
  });
}

export const chatCompletions = (c: Context) => handle(c, "chatComplete");
export const completions = (c: Context) => handle(c, "complete");
export const embeddings = (c: Context) => handle(c, "embed");
export const messages = (c: Context) => handle(c, "messages");
// POST /v1/responses — OpenAI Responses API, native passthrough. openai->openai
// request shaping is identity; no response transform => verbatim relay.
export const responses = (c: Context) => handle(c, "createModelResponse");

/** GET /v1/models — relay the catalog from the control plane. */
export const models = async (): Promise<Response> => {
  const r = await fetchCatalog();
  return new Response(r.body, {
    status: r.status,
    headers: { "content-type": "application/json" },
  });
};

/** GET /v1/models/:namespace — relay a namespace-scoped catalog from control. */
export const namespaceModels = async (c: Context): Promise<Response> => {
  const ns = c.req.param("namespace") ?? "";
  const r = await fetchCatalog(`/models/${encodeURIComponent(ns)}`);
  return new Response(r.body, {
    status: r.status,
    headers: { "content-type": "application/json" },
  });
};

/** GET /v1/models/providers/:provider — relay a provider-scoped catalog. */
export const providerModels = async (c: Context): Promise<Response> => {
  const provider = c.req.param("provider") ?? "";
  const r = await fetchCatalog(
    `/models/providers/${encodeURIComponent(provider)}`,
  );
  return new Response(r.body, {
    status: r.status,
    headers: { "content-type": "application/json" },
  });
};

/** GET /v1/embeddings/models — relay the embedding catalog from control. */
export const embeddingModels = async (): Promise<Response> => {
  const r = await fetchCatalog("/embeddings/models");
  return new Response(r.body, {
    status: r.status,
    headers: { "content-type": "application/json" },
  });
};
