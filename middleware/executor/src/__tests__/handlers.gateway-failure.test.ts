import { jest } from "@jest/globals";

const consultPre = jest.fn<() => Promise<any>>();
const consultPost = jest.fn();
const fetchCatalog = jest.fn();
const forwardToBackend = jest.fn<() => Promise<Response>>();

jest.unstable_mockModule("../integrations/control", () => ({
  consultPre,
  consultPost,
  fetchCatalog,
  hashApiKey: jest.fn(() => "hashed-key"),
}));

jest.unstable_mockModule("../integrations/backend", () => ({
  forwardToBackend,
}));

const { app } = await import("../app");

const request = () =>
  app.request("/v1/chat/completions", {
    method: "POST",
    headers: {
      authorization: "Bearer sk-test",
      "content-type": "application/json",
      "x-private-ai-gateway-request-id": "req_gateway_failure",
    },
    body: JSON.stringify({ model: "model-a", messages: [] }),
  });

describe("executor gateway-failure request logs", () => {
  beforeEach(() => {
    consultPre.mockReset();
    consultPost.mockReset();
    fetchCatalog.mockReset();
    forwardToBackend.mockReset();
  });

  it("reports consultPre 5xx denials as control-source gateway failures", async () => {
    consultPre.mockResolvedValue({
      allow: false,
      status: 503,
      message: "control plane unavailable",
    });

    const res = await request();

    expect(res.status).toBe(503);
    expect(forwardToBackend).not.toHaveBeenCalled();
    expect(consultPost).toHaveBeenCalledWith(
      expect.objectContaining({
        requestId: "req_gateway_failure",
        status: 503,
        selectedRouteId: null,
        requestModel: "model-a",
        usage: null,
        errorSource: "control",
      }),
    );
  });

  it("reports backend connection failures as backend-source gateway failures", async () => {
    consultPre.mockResolvedValue({
      allow: true,
      pricing: null,
      candidates: [{ routeId: "provider:model-a", format: "openai" }],
      userId: 123,
      virtualKeyId: 456,
      spendMode: "regular",
    });
    forwardToBackend.mockRejectedValue(new Error("backend socket missing"));

    const res = await request();

    expect(res.status).toBe(502);
    expect(consultPost).toHaveBeenCalledWith(
      expect.objectContaining({
        requestId: "req_gateway_failure",
        status: 502,
        selectedRouteId: null,
        requestModel: "model-a",
        usage: null,
        userId: 123,
        virtualKeyId: 456,
        errorSource: "backend",
      }),
    );
  });

  it("does not record a gateway failure when the client aborts before the backend responds", async () => {
    consultPre.mockResolvedValue({
      allow: true,
      pricing: null,
      candidates: [{ routeId: "provider:model-a", format: "openai" }],
      userId: 123,
      virtualKeyId: 456,
      spendMode: "regular",
    });
    // forwardToBackend rejects because the client's signal aborted, not a real
    // backend fault.
    forwardToBackend.mockRejectedValue(
      new Error("gateway backend request aborted"),
    );
    const controller = new AbortController();
    controller.abort();

    const res = await app.request("/v1/chat/completions", {
      method: "POST",
      headers: {
        authorization: "Bearer sk-test",
        "content-type": "application/json",
        "x-private-ai-gateway-request-id": "req_aborted",
      },
      body: JSON.stringify({ model: "model-a", messages: [] }),
      signal: controller.signal,
    });

    expect(res.status).toBe(499);
    expect(consultPost).not.toHaveBeenCalled();
  });
});
