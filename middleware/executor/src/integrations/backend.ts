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

const BACKEND_IDLE_TIMEOUT_MS = Number(
  process.env.PRIVATE_AI_GATEWAY_BACKEND_IDLE_TIMEOUT_MS?.trim() || 600_000,
);

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
  /** Tier value to relay to the gateway as the x-user-tier header. */
  userTier?: string;
  /** Aborts the backend hop when the caller's request/response is abandoned. */
  signal?: AbortSignal;
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
    if (args.signal?.aborted) {
      reject(new Error("gateway backend request aborted"));
      return;
    }

    let settled = false;
    let completed = false;
    let destroyed = false;
    let idleTimer: NodeJS.Timeout | undefined;
    let backendResponse: http.IncomingMessage | undefined;

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
          ...(args.userTier ? { "x-user-tier": args.userTier } : {}),
        },
      },
      (res) => {
        backendResponse = res;
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
        const upstream = Readable.toWeb(res) as ReadableStream<Uint8Array>;
        const reader = upstream.getReader();
        const finish = () => {
          completed = true;
          cleanup();
        };
        const body = new ReadableStream<Uint8Array>({
          async pull(controller) {
            resetIdleTimer();
            try {
              const { done, value } = await reader.read();
              if (done) {
                finish();
                controller.close();
                return;
              }
              resetIdleTimer();
              controller.enqueue(value);
            } catch (error) {
              cleanup();
              controller.error(error);
            }
          },
          async cancel(reason) {
            destroyBackend(reason);
            try {
              await reader.cancel(reason);
            } catch {
              // The destroy above may already have torn down the node stream.
            }
          },
        });
        resetIdleTimer();
        settled = true;
        resolve(new Response(body, { status: res.statusCode ?? 502, headers }));
      },
    );

    const cleanup = () => {
      if (idleTimer) {
        clearTimeout(idleTimer);
        idleTimer = undefined;
      }
      args.signal?.removeEventListener("abort", onAbort);
    };

    const fail = (error: Error) => {
      cleanup();
      if (!settled) {
        settled = true;
        reject(error);
      }
    };

    const destroyBackend = (reason?: unknown) => {
      if (completed || destroyed) return;
      destroyed = true;
      cleanup();
      const error =
        reason instanceof Error
          ? reason
          : new Error(
              typeof reason === "string"
                ? reason
                : "gateway backend response was abandoned",
            );
      backendResponse?.destroy(error);
      req.destroy(error);
      fail(error);
    };

    const resetIdleTimer = () => {
      if (idleTimer) clearTimeout(idleTimer);
      idleTimer = setTimeout(() => {
        destroyBackend(
          new Error(
            `gateway backend idle timeout after ${BACKEND_IDLE_TIMEOUT_MS}ms`,
          ),
        );
      }, BACKEND_IDLE_TIMEOUT_MS);
      idleTimer.unref?.();
    };

    const onAbort = () => {
      destroyBackend(new Error("gateway backend request aborted"));
    };

    args.signal?.addEventListener("abort", onAbort, { once: true });
    req.on("socket", resetIdleTimer);
    req.on("error", fail);
    req.write(payload);
    req.end();
  });
}
