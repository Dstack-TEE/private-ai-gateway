/**
 * Client-facing error responses, shaped per downstream API surface.
 *
 * Two surfaces are served (see app.ts): OpenAI (`/v1/chat/completions`,
 * `/v1/completions`, `/v1/embeddings`, `/v1/responses`) and Anthropic
 * (`/v1/messages`). Success responses are already converted per surface; these
 * builders do the same for errors so an Anthropic SDK gets a parseable
 * `{ type:"error", error:{...} }` envelope on `/v1/messages` rather than an
 * OpenAI-shaped body.
 *
 * This is the single home for every client-facing error response: upstream
 * error mapping plus the executor's own error responses. Internal exception
 * classes (thrown, not returned) live alongside in this directory.
 */

export type Surface = "openai" | "anthropic";

// ── Surface-aware status → error.type ───────────────────────────────────────
// Only covers the statuses this gateway actually emits. Anthropic's vocabulary
// (per docs.anthropic errors) differs from OpenAI's: e.g. 402 is billing_error,
// server errors are api_error (no 502/503 in Anthropic's scheme).

export function errorType(surface: Surface, status: number): string {
  if (surface === "anthropic") {
    switch (status) {
      case 400:
        return "invalid_request_error";
      case 401:
        return "authentication_error";
      case 402:
        return "billing_error";
      case 403:
        return "permission_error";
      case 404:
        return "not_found_error";
      case 429:
        return "rate_limit_error";
      case 504:
        return "timeout_error";
      default:
        return status >= 500 ? "api_error" : "invalid_request_error";
    }
  }
  switch (status) {
    case 401:
      return "authentication_error";
    case 402:
      return "insufficient_quota";
    case 403:
      return "permission_error";
    case 404:
      return "not_found_error";
    case 429:
      return "rate_limit_error";
    case 503:
      return "service_unavailable";
    case 504:
      return "timeout_error";
    default:
      return status >= 500 ? "upstream_error" : "invalid_request_error";
  }
}

// ── Upstream-error normalization (status mapping + sanitized message) ─────────
// Hides upstream provider detail. The status mapping is uniform across surfaces
// (clients branch on status class); only the envelope and error.type are
// surface-aware.

function mapUpstreamStatus(status: number): number {
  switch (status) {
    case 400:
    case 404:
    case 422:
      return status;
    case 429:
      return 429;
    case 503:
      return 503;
    case 504:
      return 504;
    default:
      return 502;
  }
}

function upstreamMessage(upstreamStatus: number): string {
  switch (upstreamStatus) {
    case 401:
    case 402:
    case 403:
      return "The upstream provider is currently unavailable";
    case 429:
      return "Rate limit exceeded. Please retry after some time.";
    case 503:
      return "The model is currently unavailable. Please try again later.";
    case 504:
      return "The upstream provider timed out";
    default:
      return "The upstream provider returned an error";
  }
}

// 4xx other than auth/billing/rate-limit (401/402/403/429) describe a problem
// with the caller's own request, so the provider's message is actionable and
// worth surfacing — but always re-wrapped in our envelope, never by returning
// the raw upstream Response (its headers must not leak).
function isActionableClientError(status: number): boolean {
  return (
    status >= 400 && status < 500 && ![401, 402, 403, 429].includes(status)
  );
}

async function tryParseJsonBody(
  response: Response,
): Promise<Record<string, any> | null> {
  try {
    return await response.json();
  } catch {
    return null;
  }
}

async function discardBody(response: Response): Promise<void> {
  try {
    // Cancel rather than drain: we don't read this body, and cancelling closes
    // the backend hop immediately instead of waiting out a stalled body.
    await response.body?.cancel();
  } catch {
    // Best effort: closing the hop is what matters.
  }
}

/** Pull a human-readable message from an upstream error body, if present. */
function extractErrorMessage(body: Record<string, any> | null): string | null {
  if (!body) return null;
  const err = body.error;
  if (typeof err === "string") return err;
  if (err && typeof err.message === "string") return err.message;
  return null;
}

// ── Envelope + builders ──────────────────────────────────────────────────────

function envelope(
  surface: Surface,
  type: string,
  message: string,
  requestId?: string,
): Record<string, unknown> {
  if (surface === "anthropic") {
    return {
      type: "error",
      error: { type, message },
      ...(requestId ? { request_id: requestId } : {}),
    };
  }
  return { error: { message, type } };
}

function jsonResponse(
  body: Record<string, unknown>,
  status: number,
  extraHeaders?: Record<string, string>,
): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", ...extraHeaders },
  });
}

/** Build a client-facing error response in the right envelope for `surface`. */
export function errorResponse(
  surface: Surface,
  status: number,
  type: string,
  message: string,
  requestId?: string,
): Response {
  return jsonResponse(envelope(surface, type, message, requestId), status);
}

/** A 429 response carrying the standard rate-limit headers. */
export function rateLimitResponse(
  surface: Surface,
  message: string,
  limit: number,
  resetAt: number,
  requestId?: string,
): Response {
  const retryAfter = Math.max(1, resetAt - Math.floor(Date.now() / 1000));
  const body = envelope(surface, "rate_limit_error", message, requestId);
  // OpenAI clients expect a string error code on rate limits.
  if (surface === "openai") {
    (body.error as Record<string, unknown>).code = "rate_limit_exceeded";
  }
  return jsonResponse(body, 429, {
    "X-RateLimit-Limit": String(limit),
    "X-RateLimit-Remaining": "0",
    "X-RateLimit-Reset": String(resetAt),
    "Retry-After": String(retryAfter),
  });
}

/**
 * Normalize a non-2xx upstream response into a surface-shaped error. The raw
 * upstream Response is never returned — status, body, and headers are always
 * rebuilt by us so provider internals (auth/billing detail, server headers)
 * can't leak. For actionable client errors (4xx other than 401/402/403/429)
 * the provider's own message is re-wrapped in our envelope (it tells the caller
 * what to fix); everything else (401/402/403/429/5xx) gets a generic sanitized
 * message so a provider's own 402/5xx never reaches the client as-is.
 */
export async function normalizeUpstreamError(
  response: Response,
  surface: Surface,
  requestId?: string,
): Promise<Response> {
  if (response.ok) {
    return response;
  }

  const upstreamStatus = response.status;
  if (isActionableClientError(upstreamStatus)) {
    const message = extractErrorMessage(await tryParseJsonBody(response));
    if (message) {
      return errorResponse(
        surface,
        upstreamStatus,
        errorType(surface, upstreamStatus),
        message,
        requestId,
      );
    }
  } else {
    await discardBody(response);
  }

  const status = mapUpstreamStatus(upstreamStatus);
  return errorResponse(
    surface,
    status,
    errorType(surface, status),
    upstreamMessage(upstreamStatus),
    requestId,
  );
}
