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

// One keep-alive connection so TLS is negotiated once, not per request.
const agent = CONTROL_URL
  ? new URL(CONTROL_URL).protocol === "https:"
    ? new https.Agent({ keepAlive: true })
    : new http.Agent({ keepAlive: true })
  : undefined;

function controlRequest(
  method: string,
  path: string,
  body?: string,
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
        agent,
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
  engine?: 'sglang' | 'vllm';
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
    );
    if (res.status !== 200) {
      return {
        allow: false,
        status: 503,
        message: "control plane unavailable",
      };
    }
    return JSON.parse(res.body) as PreConsult;
  } catch {
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
}

/**
 * Post-request consult: fire-and-forget usage report. Billing is best-effort —
 * a control-plane hiccup must never fail the user's already-served response.
 */
export function consultPost(report: PostReport): void {
  controlRequest("POST", "/consult/post", JSON.stringify(report)).catch(() => {
    /* best-effort */
  });
}

/** Fetch the model catalog (relayed by the executor's GET /v1/models). */
export function fetchCatalog(): Promise<{ status: number; body: string }> {
  return controlRequest("GET", "/models");
}
