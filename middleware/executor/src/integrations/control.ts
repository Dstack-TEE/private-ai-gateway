import { createHash } from "node:crypto";
import http from "node:http";
import https from "node:https";

import { PricingConfig } from "../services/pricing";

// The executor reaches the control plane over HTTP(S) at
// PRIVATE_AI_GATEWAY_CONTROL_URL, authenticated with a bearer token; use TLS in
// production. The consult payloads carry only {apiKeyHash, model, provider} and
// usage counts — `provider` is a routing hint, not content.
const CONTROL_URL = process.env.PRIVATE_AI_GATEWAY_CONTROL_URL?.trim();
const CONTROL_TOKEN = process.env.PRIVATE_AI_GATEWAY_CONTROL_TOKEN?.trim();

function controlRequest(
  method: string,
  path: string,
  body?: string,
  signal?: AbortSignal,
): Promise<{ status: number; body: string }> {
  if (!CONTROL_URL) {
    return Promise.reject(
      new Error("PRIVATE_AI_GATEWAY_CONTROL_URL is not set"),
    );
  }
  const payload = body === undefined ? undefined : Buffer.from(body);
  const url = new URL(CONTROL_URL);
  url.pathname = url.pathname.replace(/\/$/, "") + path; // path begins with '/'
  const lib = url.protocol === "https:" ? https : http;
  return new Promise((resolve, reject) => {
    const req = lib.request(
      url,
      {
        method,
        // No connection pooling: a fresh socket per request avoids reusing a
        // keep-alive connection the control plane / LB may have already closed
        // (Node's globalAgent would otherwise pool it).
        agent: false,
        signal,
        headers: {
          "content-type": "application/json",
          ...(CONTROL_TOKEN
            ? { authorization: `Bearer ${CONTROL_TOKEN}` }
            : {}),
          ...(payload ? { "content-length": payload.byteLength } : {}),
        },
      },
      (res) => {
        let b = "";
        res.on("data", (c) => (b += c));
        res.on("aborted", () =>
          reject(new Error(`control response aborted: ${method} ${path}`)),
        );
        res.on("error", reject);
        res.on("close", () => {
          if (!res.complete) {
            reject(
              new Error(`control response closed early: ${method} ${path}`),
            );
          }
        });
        res.on("end", () =>
          resolve({ status: res.statusCode ?? 502, body: b }),
        );
      },
    );
    req.on("error", reject);
    if (payload) req.write(payload);
    req.end();
  });
}

export type SpendMode = "regular" | "subscription" | "subscription_overflow";

export type Format = "openai" | "anthropic";

/** One ordered failover candidate: a backend route id and the upstream format. */
export interface RouteCandidate {
  /** `<provider>:<public model id>`, aligned with the backend's upstreams.json. */
  routeId: string;
  /** Which API format shapes the request / parses the response. */
  format: Format;
  /**
   * Serving engine of this upstream when it is a self-hosted OpenAI-compatible
   * server (sglang/vllm). Selects engine-specific request shaping. Absent for
   * managed third-party APIs.
   */
  engine?: "sglang" | "vllm";
}

export interface PreConsult {
  allow: boolean;
  // Set when allow is false: the status + message to return to the client.
  status?: number;
  message?: string;
  pricing?: PricingConfig | null;
  // Ordered backend route candidates for this model (the routing decision).
  candidates?: RouteCandidate[];
  // Billing identity, carried to the post-request consult.
  userId?: number;
  virtualKeyId?: number;
  spendMode?: SpendMode;
  // Tier to forward to the upstream as the x-user-tier header.
  userTier?: string;
  // Set on a 429 denial: drives the X-RateLimit-* / Retry-After headers.
  rateLimit?: { limit: number; resetAt: number };
}

/** SHA-256 hex of the bearer key — only the hash crosses to the control plane. */
export function hashApiKey(apiKey: string): string {
  return createHash("sha256").update(apiKey).digest("hex");
}

/** Provider routing block, forwarded verbatim to control. */
export interface ProviderRouting {
  only?: string[];
  order?: string[];
  allow_fallbacks?: boolean;
}

// Timeout for control-plane HTTP requests — never make an unbounded control call.
// Wired to `consultPre` here (it is on the request's critical path and fails
// closed, so a degraded control plane fails the request fast instead of leaving
// requests hanging and piling up). Generous by default — it only guards against
// an indefinite hang, not normal latency; tune via the env var.
const CONTROL_TIMEOUT_MS = Number(
  process.env.PRIVATE_AI_GATEWAY_CONTROL_TIMEOUT_MS?.trim() || 60_000,
);
// `consultPost` is fire-and-forget (off the critical path), so it gets a much
// shorter timeout: it only bounds a hung control plane so abandoned posts don't
// pile up sockets/promises under load. Still NO retry — a stalled post is dropped
// (best-effort), never resent (re-sending a report the control plane may have
// already processed would record it twice; the report is not idempotent).
const CONTROL_POST_TIMEOUT_MS = Number(
  process.env.PRIVATE_AI_GATEWAY_CONTROL_POST_TIMEOUT_MS?.trim() || 10_000,
);

/**
 * Pre-request consult: {apiKeyHash?, model, provider?} -> {allow, ...}.
 * Because this gates authorization, it fails CLOSED — an unreachable control
 * plane blocks the request (503) rather than letting it through unauthorized.
 */
export async function consultPre(
  model: string | undefined,
  apiKeyHash: string | undefined,
  provider?: ProviderRouting,
): Promise<PreConsult> {
  try {
    const res = await controlRequest(
      "POST",
      "/consult/pre",
      JSON.stringify({ apiKeyHash, model, provider }),
      AbortSignal.timeout(CONTROL_TIMEOUT_MS),
    );
    if (res.status !== 200) {
      console.error(
        `[control] /consult/pre returned HTTP ${res.status}: ${res.body.slice(0, 300)}`,
      );
      return {
        allow: false,
        status: 503,
        message: "control plane unavailable",
      };
    }
    try {
      return JSON.parse(res.body) as PreConsult;
    } catch (err) {
      console.error(
        `[control] /consult/pre returned invalid JSON: ${String(err)}; body=${res.body.slice(0, 300)}`,
      );
      return {
        allow: false,
        status: 503,
        message: "control plane unavailable",
      };
    }
  } catch (err) {
    console.error(`[control] /consult/pre request failed: ${String(err)}`);
    return { allow: false, status: 503, message: "control plane unavailable" };
  }
}

/** Post-request usage report (drives billing + request logs). */
export interface PostReport {
  requestId: string;
  endpoint: string;
  status: number;
  durationMs: number;
  ttftMs?: number;
  isStreaming?: boolean;
  attemptIndex?: number;
  // `<provider>:<model>` from the backend's selected-route header.
  selectedRouteId: string | null;
  requestModel: string;
  // Raw upstream usage (before usage.cost was injected), or null if none.
  usage: Record<string, unknown> | null;
  pricing: PricingConfig | null;
  spendMode?: SpendMode;
  userId?: number;
  virtualKeyId?: number;
  // Set only on a gateway-synthesized failure (no real upstream attempt): which
  // component produced it. Empty/absent for real upstream attempts. Drives the
  // control plane's `error_source` column.
  errorSource?: "control" | "backend" | "executor";
  errorMessage?: string;
}

/**
 * Post-request consult: fire-and-forget usage report. Delivery is best-effort — a
 * control-plane hiccup must never fail the user's already-served response, and we
 * never retry: re-sending a report the control plane already processed would
 * record it twice (the report is not idempotent). Bounded by a short timeout so a
 * hung control plane can't accumulate sockets/promises.
 */
export function consultPost(report: PostReport): void {
  controlRequest(
    "POST",
    "/consult/post",
    JSON.stringify(report),
    AbortSignal.timeout(CONTROL_POST_TIMEOUT_MS),
  ).catch((err) => {
    console.error(`[control] /consult/post request failed: ${String(err)}`);
  });
}

/** Fetch a model catalog (relayed by the executor's GET /v1/models* routes). */
export function fetchCatalog(
  path: string = "/models",
): Promise<{ status: number; body: string }> {
  return controlRequest("GET", path);
}
