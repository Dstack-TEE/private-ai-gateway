import {
  errorResponse,
  normalizeUpstreamError,
  rateLimitResponse,
  type Surface,
} from "../response";

const jsonResponse = (status: number, body: unknown): Response =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });

describe("normalizeUpstreamError — OpenAI surface", () => {
  const surface: Surface = "openai";

  it("maps upstream 402 to 502 (no leak of provider 402)", async () => {
    const upstream = jsonResponse(402, {
      error: { message: "provider out of credits" },
    });
    const res = await normalizeUpstreamError(upstream, surface);
    expect(res.status).toBe(502);
    expect(upstream.bodyUsed).toBe(true);
    const body = (await res.json()) as {
      error: { message: string; type: string };
    };
    expect(body).toEqual({
      error: {
        message: "The upstream provider is currently unavailable",
        type: "upstream_error",
      },
    });
  });

  it.each([401, 403])("maps upstream %i to 502", async (status) => {
    const res = await normalizeUpstreamError(jsonResponse(status, {}), surface);
    expect(res.status).toBe(502);
    expect(((await res.json()) as any).error.type).toBe("upstream_error");
  });

  it("maps upstream 429 to 429 rate_limit_error", async () => {
    const res = await normalizeUpstreamError(jsonResponse(429, {}), surface);
    expect(res.status).toBe(429);
    const body = (await res.json()) as {
      error: { message: string; type: string };
    };
    expect(body.error.type).toBe("rate_limit_error");
    expect(body.error.message).toBe(
      "Rate limit exceeded. Please retry after some time.",
    );
  });

  it.each([
    [400, "invalid_request_error"],
    [404, "not_found_error"],
    [422, "invalid_request_error"],
  ])(
    "surfaces an actionable client error %i message but re-wraps it (no raw passthrough, headers stripped)",
    async (status, type) => {
      const upstream = new Response(
        JSON.stringify({
          error: { message: "bad param", type: "provider_specific" },
        }),
        {
          status,
          headers: {
            "content-type": "application/json",
            "x-upstream-secret": "leak",
          },
        },
      );
      const res = await normalizeUpstreamError(upstream, surface);
      expect(res.status).toBe(status);
      expect(upstream.bodyUsed).toBe(true);
      // upstream message is surfaced, but the type is ours and the upstream
      // header never reaches the client.
      expect(await res.json()).toEqual({
        error: { message: "bad param", type },
      });
      expect(res.headers.get("x-upstream-secret")).toBeNull();
    },
  );

  it("maps a 400 lacking an error body to the standard shape", async () => {
    const res = await normalizeUpstreamError(
      jsonResponse(400, { message: "no error key" }),
      surface,
    );
    expect(res.status).toBe(400);
    expect(((await res.json()) as any).error.type).toBe(
      "invalid_request_error",
    );
  });

  it.each([
    [500, 502, "upstream_error"],
    [502, 502, "upstream_error"],
    [503, 503, "service_unavailable"],
    [504, 504, "timeout_error"],
  ])("maps upstream %i to %i (%s)", async (upstream, expected, type) => {
    const res = await normalizeUpstreamError(
      jsonResponse(upstream, {}),
      surface,
    );
    expect(res.status).toBe(expected);
    expect(((await res.json()) as any).error.type).toBe(type);
  });

  it("returns a 2xx response untouched", async () => {
    const ok = jsonResponse(200, { id: "chatcmpl-1" });
    expect(await normalizeUpstreamError(ok, surface)).toBe(ok);
  });
});

describe("normalizeUpstreamError — Anthropic surface", () => {
  const surface: Surface = "anthropic";

  it("maps upstream 402 to 502 with Anthropic envelope + request_id", async () => {
    const res = await normalizeUpstreamError(
      jsonResponse(402, { error: { message: "provider out of credits" } }),
      surface,
      "req_123",
    );
    expect(res.status).toBe(502);
    expect(await res.json()).toEqual({
      type: "error",
      error: {
        type: "api_error",
        message: "The upstream provider is currently unavailable",
      },
      request_id: "req_123",
    });
  });

  it("maps upstream 429 to 429 rate_limit_error in Anthropic envelope", async () => {
    const res = await normalizeUpstreamError(jsonResponse(429, {}), surface);
    expect(res.status).toBe(429);
    const body = (await res.json()) as {
      type: string;
      error: { type: string };
    };
    expect(body.type).toBe("error");
    expect(body.error.type).toBe("rate_limit_error");
  });

  it("re-wraps an actionable client error into the Anthropic envelope (no raw passthrough, headers stripped)", async () => {
    // Upstream returns an OpenAI-shaped error; the client called /v1/messages
    // so it must come back in the Anthropic envelope, headers stripped.
    const upstream = new Response(
      JSON.stringify({ error: { message: "bad param", type: "provider_x" } }),
      {
        status: 404,
        headers: {
          "content-type": "application/json",
          "x-upstream-secret": "leak",
        },
      },
    );
    const res = await normalizeUpstreamError(upstream, surface, "req_9");
    expect(res.status).toBe(404);
    expect(await res.json()).toEqual({
      type: "error",
      error: { type: "not_found_error", message: "bad param" },
      request_id: "req_9",
    });
    expect(res.headers.get("x-upstream-secret")).toBeNull();
  });

  it("falls back to a generic message when the client error has no message", async () => {
    const res = await normalizeUpstreamError(
      jsonResponse(400, { detail: "no error field" }),
      surface,
    );
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({
      type: "error",
      error: {
        type: "invalid_request_error",
        message: "The upstream provider returned an error",
      },
    });
  });
});

describe("errorResponse — executor-generated errors per surface", () => {
  it("OpenAI: model_not_found 400", async () => {
    const res = errorResponse(
      "openai",
      400,
      "model_not_found",
      "no route",
      "req_1",
    );
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({
      error: { message: "no route", type: "model_not_found" },
    });
  });

  it("Anthropic: account quota 402 → billing envelope with request_id", async () => {
    const res = errorResponse(
      "anthropic",
      402,
      "billing_error",
      "add credits",
      "req_2",
    );
    expect(res.status).toBe(402);
    expect(await res.json()).toEqual({
      type: "error",
      error: { type: "billing_error", message: "add credits" },
      request_id: "req_2",
    });
  });
});

describe("rateLimitResponse", () => {
  it("OpenAI: 429 with rate-limit headers and string code", async () => {
    const res = rateLimitResponse("openai", "slow down", 100, 9999999999);
    expect(res.status).toBe(429);
    expect(res.headers.get("X-RateLimit-Limit")).toBe("100");
    expect(res.headers.get("Retry-After")).toBeTruthy();
    const body = (await res.json()) as {
      error: { type: string; code: string };
    };
    expect(body.error.type).toBe("rate_limit_error");
    expect(body.error.code).toBe("rate_limit_exceeded");
  });

  it("Anthropic: 429 in error envelope, headers present", async () => {
    const res = rateLimitResponse(
      "anthropic",
      "slow down",
      100,
      9999999999,
      "req_3",
    );
    expect(res.status).toBe(429);
    expect(res.headers.get("X-RateLimit-Reset")).toBe("9999999999");
    expect(await res.json()).toEqual({
      type: "error",
      error: { type: "rate_limit_error", message: "slow down" },
      request_id: "req_3",
    });
  });
});
