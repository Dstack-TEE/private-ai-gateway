import http from "node:http";
import { Readable } from "node:stream";

const DEFAULT_BACKEND_SOCKET = "/run/private-ai-gateway/backend.sock";

// Per-hop framing/connection headers. They describe the backend->executor
// hop only; re-emitting them would conflict with the framing the executor's
// own server computes for the body it streams to the frontend.
const HOP_BY_HOP_HEADERS = new Set([
  "connection",
  "keep-alive",
  "transfer-encoding",
  "content-length",
  "upgrade",
  "te",
  "trailer",
  "proxy-authenticate",
  "proxy-authorization",
]);

function backendSocketPath(): string {
  return (
    process.env.PRIVATE_AI_GATEWAY_BACKEND_UDS_PATH?.trim() ||
    DEFAULT_BACKEND_SOCKET
  );
}

export interface ForwardArgs {
  /** Request id minted by the frontend; the backend looks up the stored request by it. */
  requestId: string;
  /** Ordered candidate route ids; the backend tries them in order until one commits. */
  targets: string[];
  /**
   * Provider-format request bytes. Same-format candidates share one body;
   * mixed-format candidates pass a `{ "candidates": [...] }` envelope whose
   * target order matches `targets`.
   */
  body: Uint8Array | string;
}

/**
 * Dial the gateway backend over its Unix domain socket and POST
 * `/internal/forward`. Returns the backend response with its body stream
 * intact (SSE or buffered) and its semantic headers (`x-receipt-id`, the
 * route-attribution headers, content-type, ...) preserved; per-hop framing
 * headers are dropped so the executor's server can frame the re-emitted body.
 */
export function forwardToBackend(args: ForwardArgs): Promise<Response> {
  const payload = Buffer.from(args.body);
  return new Promise((resolve, reject) => {
    const req = http.request(
      {
        socketPath: backendSocketPath(),
        path: "/internal/forward",
        method: "POST",
        headers: {
          "content-type": "application/json",
          "content-length": payload.byteLength,
          "x-private-ai-gateway-request-id": args.requestId,
          "x-private-ai-gateway-targets": args.targets.join(","),
        },
      },
      (res) => {
        // Relay semantic headers (content-type, x-receipt-id, the
        // x-private-ai-gateway-* attribution headers, cache-control,
        // x-accel-buffering, ...); drop per-hop framing headers and let the
        // executor's server frame the body it re-emits.
        const headers = new Headers();
        for (const [name, value] of Object.entries(res.headers)) {
          if (value === undefined) continue;
          if (HOP_BY_HOP_HEADERS.has(name.toLowerCase())) continue;
          headers.set(
            name,
            Array.isArray(value) ? value.join(", ") : String(value),
          );
        }
        const body = Readable.toWeb(res) as ReadableStream<Uint8Array>;
        resolve(new Response(body, { status: res.statusCode ?? 502, headers }));
      },
    );
    req.on("error", reject);
    req.write(payload);
    req.end();
  });
}
