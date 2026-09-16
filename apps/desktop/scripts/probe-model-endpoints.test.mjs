import assert from "node:assert/strict";
import { test } from "node:test";
import { payload, probe, summarizeStream, toolResultPayload } from "./probe-model-endpoints.mjs";

const surfaces = ["responses", "messages", "chat/completions"];
const args = '{"value":"OK"}';

function events(surface, tools = true) {
  if (surface === "responses") {
    const item = tools
      ? { type: "function_call", id: "fc-1", call_id: "call-1", name: "probe_echo", arguments: args }
      : { type: "message", role: "assistant", content: [{ type: "output_text", text: "OK" }] };
    return tools ? [
      { type: "response.output_item.added", output_index: 0, item: { ...item, arguments: "" } },
      { type: "response.function_call_arguments.delta", output_index: 0, delta: args },
      { type: "response.output_item.done", output_index: 0, item },
      { type: "response.completed", response: { object: "response", status: "completed", output: [item] } },
    ] : [
      { type: "response.output_text.delta", delta: "OK" },
      { type: "response.completed", response: { object: "response", status: "completed", output: [item] } },
    ];
  }
  if (surface === "messages") return [
    { type: "message_start", message: { type: "message", role: "assistant", content: [] } },
    { type: "content_block_start", index: 0, content_block: tools ? { type: "tool_use", id: "call-1", name: "probe_echo", input: {} } : { type: "text", text: "" } },
    { type: "content_block_delta", index: 0, delta: tools ? { type: "input_json_delta", partial_json: args } : { type: "text_delta", text: "OK" } },
    { type: "message_delta", delta: { stop_reason: tools ? "tool_use" : "end_turn" } },
    { type: "message_stop" },
  ];
  return [
    { choices: [{ index: 0, delta: tools ? { tool_calls: [{ index: 0, id: "call-1", type: "function", function: { name: "probe_echo", arguments: args } }] } : { content: "OK" } }] },
    { choices: [{ index: 0, delta: {}, finish_reason: tools ? "tool_calls" : "stop" }] },
    { type: "done" },
  ];
}

function sse(data) {
  const bytes = new TextEncoder().encode(data.map(event => `data: ${event.type === "done" ? "[DONE]" : JSON.stringify(event)}\n\n`).join(""));
  let cursor = 0;
  return new Response(new ReadableStream({
    pull(controller) {
      if (cursor === bytes.length) { controller.close(); return; }
      controller.enqueue(bytes.slice(cursor, cursor + 7));
      cursor = Math.min(cursor + 7, bytes.length);
    },
  }), { headers: { "content-type": "text/event-stream; charset=utf-8" } });
}

test("tool requests and result turns use each API's native contract", () => {
  for (const surface of surfaces) {
    const request = payload("model-a", surface, { stream: true, tools: true });
    assert.equal(request.stream, true);
    assert.deepEqual(request.tool_choice, surface === "messages" ? { type: "auto" } : "auto");
    if (surface !== "messages") {
      assert.equal(request.parallel_tool_calls, true);
      const tool = surface === "responses" ? request.tools[0] : request.tools[0].function;
      assert.equal(tool.strict, false);
    }
    const summary = summarizeStream(surface, events(surface));
    assert.equal(summary.streaming.status, "supported");
    assert.equal(summary.tools.status, "supported");
    const resumed = toolResultPayload("model-a", surface, summary.body);
    assert.equal(resumed.stream, true);
    assert.deepEqual(resumed.tool_choice, surface === "messages" ? { type: "auto" } : "auto");
    const input = JSON.stringify(resumed.input ?? resumed.messages);
    assert.match(input, /call-1/);
    assert.match(input, /OK/);
  }
  const sparse = summarizeStream("messages", events("messages").map(event =>
    event.type === "content_block_start" || event.type === "content_block_delta" ? { ...event, index: 1 } : event));
  assert.equal(sparse.tools.status, "supported");
  assert.doesNotThrow(() => toolResultPayload("model-a", "messages", sparse.body));
});

test("truncated streams, invalid event order, and ignored automatic tool choice stay inconclusive", () => {
  for (const surface of surfaces) {
    assert.equal(summarizeStream(surface, events(surface).slice(0, -1)).streaming.status, "inconclusive");
    assert.equal(summarizeStream(surface, events(surface, false)).tools.status, "inconclusive");
  }
  assert.equal(summarizeStream("responses", events("responses").slice(1)).streaming.status, "inconclusive");
  const invalid = events("messages");
  invalid[1].index = 2 ** 32 - 2;
  assert.equal(summarizeStream("messages", invalid).streaming.status, "inconclusive");
});

test("full probes consume chunked SSE and publish only observations, not response text or credentials", async () => {
  const calls = [];
  const report = await probe({
    endpoint: "https://tee.redpill.ai", key: "test-private-key", concurrency: 1,
    fetchImpl: async (url, options) => {
      if (url.endsWith("/models")) return Response.json({ data: [{ id: "model-a" }] });
      const surface = url.split("/v1/")[1];
      const body = JSON.parse(options.body);
      calls.push({ surface, body });
      if (!body.stream) return Response.json(surface === "responses" ? { object: "response", status: "completed", output: [] }
        : surface === "messages" ? { type: "message", role: "assistant", content: [] } : { choices: [{ message: { role: "assistant", content: "OK" } }] });
      const resumed = (body.input ?? body.messages).length > 1;
      return sse(events(surface, Boolean(body.tools) && !resumed));
    },
  });
  assert.equal(calls.length, 9);
  assert.equal(report.schemaVersion, 2);
  for (const result of report.results) {
    assert.equal(result.status, "supported");
    assert.deepEqual(Object.values(result.checks).map(check => check.status), ["supported", "supported", "supported"]);
  }
  assert.doesNotMatch(JSON.stringify(report), /test-private-key|call-1|probe_echo/);
});

test("plain streaming is checked separately when a tool request is rejected", async () => {
  const report = await probe({
    endpoint: "https://tee.redpill.ai", key: "test-key", concurrency: 1,
    modelIds: ["model-a"], surfaceNames: ["responses"],
    fetchImpl: async (url, options) => {
      if (url.endsWith("/models")) return Response.json({ data: [{ id: "model-a" }] });
      const body = JSON.parse(options.body);
      if (!body.stream) return Response.json({ object: "response", status: "completed", output: [] });
      if (body.tools) return Response.json({ error: { code: "invalid_request" } }, { status: 400 });
      return sse(events("responses", false));
    },
  });
  assert.equal(report.results[0].checks.streaming.status, "supported");
  assert.equal(report.results[0].checks.tools.status, "inconclusive");
  assert.equal(report.results[0].checks.tools.reason, "request_rejected");
});

test("an auth failure during streaming stops further requests and leaves checks inconclusive", async () => {
  let requests = 0;
  const report = await probe({
    endpoint: "https://tee.redpill.ai", key: "test-key", concurrency: 1,
    fetchImpl: async (url) => {
      if (url.endsWith("/models")) return Response.json({ data: [{ id: "model-a" }] });
      requests++;
      return requests === 1 ? Response.json({ choices: [{ message: { role: "assistant" } }] })
        : Response.json({ error: { message: "private provider detail" } }, { status: 401 });
    },
  });
  assert.equal(requests, 2);
  assert.equal(report.results[0].checks.streaming.reason, "authentication_or_permission");
  assert.equal(report.results[1].reason, "not_probed_after_authentication_error");
  assert.doesNotMatch(JSON.stringify(report), /private provider detail/);
});

test("rate limits count as compatibility and later models are still probed", async () => {
  let requests = 0;
  const report = await probe({
    endpoint: "https://tee.redpill.ai", key: "test-key", concurrency: 1, basic: true,
    fetchImpl: async (url) => {
      if (url.endsWith("/models")) return Response.json({ data: [{ id: "model-a" }, { id: "model-b" }] });
      requests++;
      if (requests === 1) return Response.json({ error: { code: "rate_limit" } }, { status: 429 });
      const surface = url.split("/v1/")[1];
      return Response.json(surface === "responses" ? { object: "response", status: "completed", output: [] }
        : surface === "messages" ? { type: "message", role: "assistant", content: [] }
          : { choices: [{ message: { role: "assistant", content: "OK" } }] });
    },
  });
  assert.equal(requests, 6);
  assert.equal(report.results[0].status, "supported");
  assert.equal(report.results[0].reason, "temporary_rate_or_quota_limit");
  assert.equal(report.results.at(-1).status, "supported");
});

test("temporary server responses keep agent surfaces available", async () => {
  const report = await probe({
    endpoint: "https://tee.redpill.ai", key: "test-key", concurrency: 1,
    modelIds: ["model-a"], surfaceNames: ["responses"],
    fetchImpl: async url => url.endsWith("/models")
      ? Response.json({ data: [{ id: "model-a" }] })
      : Response.json({ error: { code: "upstream_unavailable" } }, { status: 503 }),
  });
  assert.equal(report.results[0].status, "supported");
  assert.equal(report.results[0].reason, "temporary_server_error");
  assert.deepEqual(Object.values(report.results[0].checks).map(check => check.status), [
    "supported", "supported", "supported",
  ]);
});

test("inconclusive refreshes retain prior conclusive capability evidence", async () => {
  const prior = {
    schemaVersion: 2, endpoint: "https://tee.redpill.ai", checkedAt: "2026-09-15T00:00:00Z",
    results: surfaces.map(surface => ({
      model: "model-a", endpoint: `/v1/${surface}`, status: "supported", reason: "valid_response", httpStatus: 200,
      checks: {
        streaming: { status: "supported", reason: "valid_event_stream", httpStatus: 200 },
        tools: { status: "supported", reason: "valid_streamed_tool_call", httpStatus: 200 },
        toolResult: { status: "supported", reason: "valid_tool_result_response", httpStatus: 200 },
      },
    })),
  };
  const report = await probe({
    endpoint: prior.endpoint, key: "test-key", concurrency: 1, previousInventory: prior,
    fetchImpl: async (url) => url.endsWith("/models")
      ? Response.json({ data: [{ id: "model-a" }] })
      : Promise.reject(new Error("temporary network failure")),
  });
  assert.equal(report.results.length, 3);
  for (const result of report.results) {
    assert.equal(result.status, "supported");
    assert.match(result.reason, /^retained_previous_supported_after_network_timeout_or_invalid_response$/);
    assert.equal(result.checks.streaming.status, "supported");
  }
});

test("explicit endpoint incompatibility clears stale capability evidence", async () => {
  const prior = {
    schemaVersion: 2, endpoint: "https://tee.redpill.ai", checkedAt: "2026-09-15T00:00:00Z",
    results: surfaces.map(surface => ({
      model: "model-a", endpoint: `/v1/${surface}`, status: "supported", reason: "valid_response", httpStatus: 200,
      checks: {
        streaming: { status: "supported", reason: "valid_event_stream", httpStatus: 200 },
        tools: { status: "supported", reason: "valid_streamed_tool_call", httpStatus: 200 },
        toolResult: { status: "supported", reason: "valid_tool_result_response", httpStatus: 200 },
      },
    })),
  };
  const report = await probe({
    endpoint: prior.endpoint, key: "test-key", previousInventory: prior,
    modelIds: ["model-a"], surfaceNames: ["responses"],
    fetchImpl: async (url) => url.endsWith("/models")
      ? Response.json({ data: [{ id: "model-a" }] })
      : Response.json({ error: { code: "unsupported_endpoint" } }, { status: 404 }),
  });
  const result = report.results.find(entry => entry.endpoint === "/v1/responses");
  assert.equal(result.status, "unavailable");
  assert.deepEqual(Object.values(result.checks).map(check => check.status), [
    "inconclusive", "inconclusive", "inconclusive",
  ]);
});
