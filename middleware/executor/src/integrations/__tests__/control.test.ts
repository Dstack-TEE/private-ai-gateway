import http from "node:http";
import type { AddressInfo } from "node:net";

// The control client reads PRIVATE_AI_GATEWAY_CONTROL_URL / _TOKEN at module
// load, so a real loopback server is started and the env is set *before* the
// dynamic import below.
describe("control — remote dial", () => {
  let server: http.Server;
  let serverClosed = false;
  let received: { auth?: string; path?: string; body?: string }[] = [];
  let nextResponse = { status: 200, body: "{}" };
  let consultPre: typeof import("../control").consultPre;

  beforeAll(async () => {
    server = http.createServer((req, res) => {
      let b = "";
      req.on("data", (c) => (b += c));
      req.on("end", () => {
        received.push({
          auth: req.headers.authorization,
          path: req.url,
          body: b,
        });
        res.writeHead(nextResponse.status, {
          "content-type": "application/json",
        });
        res.end(nextResponse.body);
      });
    });
    await new Promise<void>((r) => server.listen(0, "127.0.0.1", () => r()));
    const port = (server.address() as AddressInfo).port;
    process.env.PRIVATE_AI_GATEWAY_CONTROL_URL = `http://127.0.0.1:${port}`;
    process.env.PRIVATE_AI_GATEWAY_CONTROL_TOKEN = "test-token";
    ({ consultPre } = await import("../control"));
  });

  afterAll(async () => {
    if (!serverClosed) await new Promise<void>((r) => server.close(() => r()));
  });

  beforeEach(() => {
    received = [];
    nextResponse = { status: 200, body: "{}" };
  });

  it("sends the Bearer token + {apiKeyHash, model} body to /consult/pre and parses the result", async () => {
    nextResponse = {
      status: 200,
      body: JSON.stringify({ allow: true, candidates: [] }),
    };
    const res = await consultPre("gpt-4o", "abc");
    expect(res.allow).toBe(true);
    expect(received).toHaveLength(1);
    expect(received[0].path).toBe("/consult/pre");
    expect(received[0].auth).toBe("Bearer test-token");
    expect(JSON.parse(received[0].body!)).toEqual({
      apiKeyHash: "abc",
      model: "gpt-4o",
    });
  });

  it("fails closed (503) on a non-200 control response", async () => {
    nextResponse = { status: 500, body: "boom" };
    const res = await consultPre("gpt-4o", "abc");
    expect(res).toEqual({
      allow: false,
      status: 503,
      message: "control plane unavailable",
    });
  });

  // Keep last: closes the server so the connection is refused.
  it("fails closed (503) when the control plane is unreachable", async () => {
    await new Promise<void>((r) => server.close(() => r()));
    serverClosed = true;
    const res = await consultPre("gpt-4o", "abc");
    expect(res).toEqual({
      allow: false,
      status: 503,
      message: "control plane unavailable",
    });
  });
});
