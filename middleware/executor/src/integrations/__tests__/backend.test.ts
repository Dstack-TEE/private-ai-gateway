import { existsSync, mkdtempSync, unlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import http from "node:http";

function waitFor<T>(promise: Promise<T>, timeoutMs = 1_000): Promise<T> {
  return Promise.race([
    promise,
    new Promise<T>((_, reject) => {
      const timer = setTimeout(
        () => reject(new Error(`timed out after ${timeoutMs}ms`)),
        timeoutMs,
      );
      timer.unref?.();
    }),
  ]);
}

async function listen(server: http.Server, socketPath: string): Promise<void> {
  if (existsSync(socketPath)) unlinkSync(socketPath);
  await new Promise<void>((resolve) => server.listen(socketPath, resolve));
}

describe("backend — UDS response lifecycle", () => {
  let forwardToBackend: typeof import("../backend").forwardToBackend;

  beforeAll(async () => {
    process.env.PRIVATE_AI_GATEWAY_BACKEND_IDLE_TIMEOUT_MS = "100";
    ({ forwardToBackend } = await import("../backend"));
  });

  it("closes the backend socket when the returned body is cancelled", async () => {
    const socketPath = path.join(
      mkdtempSync(path.join(tmpdir(), "pag-backend-")),
      "backend.sock",
    );
    let socketClosed!: Promise<void>;
    const server = http.createServer((req, res) => {
      req.resume();
      socketClosed = new Promise((resolve) =>
        req.socket.once("close", resolve),
      );
      res.writeHead(200, { "content-type": "text/event-stream" });
      res.write("data: hello\n\n");
    });

    try {
      await listen(server, socketPath);
      process.env.PRIVATE_AI_GATEWAY_BACKEND_UDS_PATH = socketPath;

      const response = await forwardToBackend({
        requestId: "req_cancel",
        targets: ["route-a"],
        body: "{}",
      });

      expect(response.status).toBe(200);
      await response.body?.cancel("client stopped reading");
      await waitFor(socketClosed);
    } finally {
      await new Promise<void>((resolve) => server.close(() => resolve()));
      if (existsSync(socketPath)) unlinkSync(socketPath);
    }
  });

  it("idle-times out a backend response body that is never consumed", async () => {
    const socketPath = path.join(
      mkdtempSync(path.join(tmpdir(), "pag-backend-")),
      "backend.sock",
    );
    let socketClosed!: Promise<void>;
    const server = http.createServer((req, res) => {
      req.resume();
      socketClosed = new Promise((resolve) =>
        req.socket.once("close", resolve),
      );
      res.writeHead(200, { "content-type": "text/event-stream" });
      res.write("data: first\n\n");
    });

    try {
      await listen(server, socketPath);
      process.env.PRIVATE_AI_GATEWAY_BACKEND_UDS_PATH = socketPath;

      const response = await forwardToBackend({
        requestId: "req_unread",
        targets: ["route-a"],
        body: "{}",
      });

      expect(response.status).toBe(200);
      expect(response.bodyUsed).toBe(false);
      await waitFor(socketClosed, 1_500);
    } finally {
      await new Promise<void>((resolve) => server.close(() => resolve()));
      if (existsSync(socketPath)) unlinkSync(socketPath);
    }
  });

  it("closes the backend socket when a consumed stream goes idle mid-read", async () => {
    const socketPath = path.join(
      mkdtempSync(path.join(tmpdir(), "pag-backend-")),
      "backend.sock",
    );
    let socketClosed!: Promise<void>;
    const server = http.createServer((req, res) => {
      req.resume();
      socketClosed = new Promise((resolve) =>
        req.socket.once("close", resolve),
      );
      res.writeHead(200, { "content-type": "text/event-stream" });
      res.write("data: first\n\n");
    });

    try {
      await listen(server, socketPath);
      process.env.PRIVATE_AI_GATEWAY_BACKEND_UDS_PATH = socketPath;

      const response = await forwardToBackend({
        requestId: "req_mid_read_idle",
        targets: ["route-a"],
        body: "{}",
      });
      const reader = response.body!.getReader();
      const first = await reader.read();
      expect(first.done).toBe(false);
      expect(new TextDecoder().decode(first.value)).toContain("data: first");

      void reader.read().catch(() => {
        // The important assertion is that the stalled backend socket closes.
      });
      await waitFor(socketClosed, 1_500);
    } finally {
      await new Promise<void>((resolve) => server.close(() => resolve()));
      if (existsSync(socketPath)) unlinkSync(socketPath);
    }
  });

  it("rejects without dialing when the signal is already aborted", async () => {
    const controller = new AbortController();
    controller.abort();
    await expect(
      forwardToBackend({
        requestId: "req_pre_abort",
        targets: ["route-a"],
        body: "{}",
        signal: controller.signal,
      }),
    ).rejects.toThrow();
  });

  it("closes the backend socket when the signal aborts after response headers", async () => {
    const socketPath = path.join(
      mkdtempSync(path.join(tmpdir(), "pag-backend-")),
      "backend.sock",
    );
    let socketClosed!: Promise<void>;
    const server = http.createServer((req, res) => {
      req.resume();
      socketClosed = new Promise((resolve) =>
        req.socket.once("close", resolve),
      );
      res.writeHead(200, { "content-type": "text/event-stream" });
      res.write("data: first\n\n");
      // never end — only the abort can release this connection
    });

    try {
      await listen(server, socketPath);
      process.env.PRIVATE_AI_GATEWAY_BACKEND_UDS_PATH = socketPath;

      const controller = new AbortController();
      const response = await forwardToBackend({
        requestId: "req_abort_after",
        targets: ["route-a"],
        body: "{}",
        signal: controller.signal,
      });
      expect(response.status).toBe(200);

      controller.abort();
      await waitFor(socketClosed);
    } finally {
      await new Promise<void>((resolve) => server.close(() => resolve()));
      if (existsSync(socketPath)) unlinkSync(socketPath);
    }
  });
});
