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
  PostReport,
  PreConsult,
  RouteCandidate,
} from "./integrations/control";
import { meterResponse, StreamOutcome } from "./cost";
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

/**
 * The status to record for a request. We log the RAW upstream status (so a 402,
 * 401, 500, ... is observable as itself, not flattened) — the client-facing
 * status is mapped separately by `normalizeUpstreamError` and is intentionally
 * not what we record. For streaming, the HTTP status commits (200) when the
 * headers flush, before the body is produced, so a failure after that point
 * can't change it; the stream `outcome` then overrides the recorded status so
 * these post-headers failures don't sit in the "200 success" bucket.
 *
 * 502 for a genuine stream failure (broke, carried an in-band/finish_reason
 * error, or was truncated). 499 (a 4xx, excluded from uptime) for a client
 * disconnect — not a server fault. Otherwise the raw upstream status stands.
 */
function meteredStatus(
  outcome: StreamOutcome | undefined,
  upstreamStatus: number,
): number {
  switch (outcome) {
    case "client_closed":
      return 499;
    case "failed":
      return 502;
    default:
      return upstreamStatus;
  }
}

function safeErrorMessage(err: unknown): string {
  const message = err instanceof Error ? err.message : String(err);
  return message.slice(0, 500);
}

function reportGatewayFailure(args: {
  c: Context;
  requestId: string;
  params: Params;
  status: number;
  start: number;
  source: PostReport["errorSource"];
  message: string;
  consult?: PreConsult;
  pricing?: PostReport["pricing"];
  // Defaults to 0. Paths that fail after the backend already reported failed
  // failover attempts must pass a value above those attempts' indices, so this
  // gateway-generated row wins `argMax(status_code, attempt_index)` as the final
  // status instead of being shadowed by an earlier failed attempt.
  attemptIndex?: number;
}): void {
  consultPost({
    requestId: args.requestId,
    endpoint: new URL(args.c.req.url).pathname,
    status: args.status,
    durationMs: Date.now() - args.start,
    isStreaming: Boolean(args.params.stream),
    attemptIndex: args.attemptIndex ?? 0,
    selectedRouteId: null,
    requestModel: args.params.model ?? "",
    usage: null,
    pricing: args.pricing ?? args.consult?.pricing ?? null,
    spendMode: args.consult?.spendMode,
    userId: args.consult?.userId,
    virtualKeyId: args.consult?.virtualKeyId,
    errorSource: args.source,
    errorMessage: args.message.slice(0, 500),
  });
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
    // Record a gateway failure only when OUR infrastructure failed (5xx). The
    // other denial statuses are attributable to the user's request, not our
    // availability, so — like 400/403/413/429 — they are not recorded:
    //   401  invalid user API key
    //   402  user out of budget/credits
    //   404  user pinned a provider that has no node for the model
    // Real upstream 401/402/etc. (the provider's key/billing failed) are a
    // different thing and still count — recorded from the metered/failed-attempt
    // paths below. (Caveat: the Rust backend does not set attribution headers on
    // a FINAL upstream error, so such rows currently land with selectedRouteId
    // null and no provider/deployment attribution — a separate, pre-existing
    // backend gap.)
    if (status >= 500) {
      reportGatewayFailure({
        c,
        requestId,
        params,
        status,
        start,
        source: "control",
        message,
        consult,
      });
    }
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

  let targets: string[];
  let body: string;
  try {
    ({ targets, body } = buildForwardPayload(params, fn, candidates));
  } catch (err) {
    const message = `failed to shape provider request: ${safeErrorMessage(err)}`;
    reportGatewayFailure({
      c,
      requestId,
      params,
      status: 500,
      start,
      source: "executor",
      message,
      consult,
      pricing,
    });
    return errorResponse(
      surface,
      500,
      errorType(surface, 500),
      message,
      requestId,
    );
  }
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
    // The client's request signal is wired into forwardToBackend, so a client
    // disconnect before the backend responds also rejects here. That's a client
    // abort (499), not our failure — don't record it as a gateway/backend 502.
    if (c.req.raw.signal?.aborted) {
      return errorResponse(
        surface,
        499,
        errorType(surface, 499),
        "client closed request before the backend responded",
        requestId,
      );
    }
    const message = `failed to reach gateway backend: ${(err as Error).message}`;
    reportGatewayFailure({
      c,
      requestId,
      params,
      status: 502,
      start,
      source: "backend",
      message,
      consult,
      pricing,
    });
    return errorResponse(
      surface,
      502,
      "backend_unreachable",
      message,
      requestId,
    );
  }
  let response: Response;
  try {
    response = await driveResponse(
      backendResp,
      params,
      fn,
      candidates,
      surface,
      requestId,
    );
  } catch (err) {
    // This catches the whole response pipeline, not just executor code bugs: a
    // selected upstream returning a malformed/non-JSON 200 (or transform-
    // incompatible data) also throws here. We attribute ALL of it to the executor
    // (selectedRouteId null → deployment 0), i.e. a provider that emits malformed
    // 200s is NOT penalized in deployment/routing health. Accepted tradeoff — at
    // this point we can't cleanly separate a provider-data fault from an executor
    // bug; the request is still correctly counted as a 502 failure either way.
    const message = `failed to transform upstream response: ${safeErrorMessage(err)}`;
    reportGatewayFailure({
      c,
      requestId,
      params,
      status: 502,
      start,
      source: "executor",
      message,
      consult,
      pricing,
    });
    return errorResponse(
      surface,
      502,
      errorType(surface, 502),
      message,
      requestId,
    );
  }

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

  try {
    return await meterResponse(
      response,
      pricing,
      start,
      (usage, ttftMs, outcome) => {
        consultPost({
          requestId,
          endpoint: new URL(c.req.url).pathname,
          // Record the raw upstream status (`backendResp.status`) so upstream errors
          // like 402/401/500 are observable as themselves. The client still receives
          // the mapped status from `normalizeUpstreamError` (e.g. 402 -> generic 502);
          // we deliberately do not record that mapped value.
          status: meteredStatus(outcome, backendResp.status),
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
      },
    );
  } catch (err) {
    const message = `failed to meter upstream response: ${safeErrorMessage(err)}`;
    reportGatewayFailure({
      c,
      requestId,
      params,
      status: 502,
      start,
      source: "executor",
      message,
      consult,
      pricing,
      // This failure happens after any failed failover attempts were reported
      // (each at its own attempt_index); sit one past the committed attempt so
      // this 502 is the final status, not an earlier attempt's code.
      attemptIndex: attemptIndex + 1,
    });
    return errorResponse(
      surface,
      502,
      errorType(surface, 502),
      message,
      requestId,
    );
  }
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
