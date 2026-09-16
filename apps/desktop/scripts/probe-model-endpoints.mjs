import { parseArgs } from "node:util";
import { pathToFileURL } from "node:url";
import { readFile } from "node:fs/promises";
import { EventSourceParserStream } from "eventsource-parser/stream";

const surfaces = ["chat/completions", "responses", "messages"];
const unsupportedCodes = new Set([
  "unsupported_model", "model_not_supported", "model_not_found",
  "unsupported_endpoint", "endpoint_not_supported",
]);
const maxBodyBytes = 1024 * 1024;
const toolName = "probe_echo";
const toolParameters = {
  type: "object", properties: { value: { type: "string", enum: ["OK"] } },
  required: ["value"], additionalProperties: false,
};
const unknown = reason => ({ status: "inconclusive", reason });
const supported = reason => ({ status: "supported", reason });
const withHttpStatus = (result, httpStatus) => ({ ...result, ...(httpStatus ? { httpStatus } : {}) });

function checks(reason) {
  return {
    streaming: unknown(reason),
    tools: unknown(reason),
    toolResult: unknown(reason),
  };
}

function retainConclusive(previous, current) {
  if (!previous || current.status !== "inconclusive" || previous.status === "inconclusive") return current;
  return {
    ...previous,
    reason: `retained_previous_${previous.status}_after_${current.reason}`,
  };
}

function mergeObservation(previous, current, schemaVersion) {
  const base = retainConclusive(previous, current);
  const merged = {
    model: current.model,
    endpoint: current.endpoint,
    status: base.status,
    reason: base.reason,
    ...(base.httpStatus ? { httpStatus: base.httpStatus } : {}),
  };
  if (schemaVersion === 2) {
    const currentChecks = current.checks ?? checks("not_probed_in_version_two");
    const previousChecks = previous?.checks;
    merged.checks = Object.fromEntries(["streaming", "tools", "toolResult"].map(name => [
      name,
      current.status === "unavailable"
        ? currentChecks[name]
        : retainConclusive(previousChecks?.[name], currentChecks[name]),
    ]));
  }
  return merged;
}

export function mergeInventory(previous, current, catalogIds) {
  if (!previous || previous.endpoint !== current.endpoint || ![1, 2].includes(previous.schemaVersion) ||
      previous.schemaVersion > current.schemaVersion || !Array.isArray(previous.results) ||
      previous.reasoningEffort !== current.reasoningEffort) {
    throw new Error("The previous inventory is incompatible with this probe.");
  }
  const key = entry => `${entry.model}\n${entry.endpoint}`;
  const previousByKey = new Map(previous.results.map(entry => [key(entry), entry]));
  const currentByKey = new Map(current.results.map(entry => [key(entry), entry]));
  const results = [];
  for (const model of catalogIds) {
    const entries = surfaces.map(surface => {
      const endpoint = `/v1/${surface}`;
      const fresh = currentByKey.get(`${model}\n${endpoint}`);
      const prior = previousByKey.get(`${model}\n${endpoint}`);
      if (fresh) return mergeObservation(prior, fresh, current.schemaVersion);
      if (!prior) return undefined;
      return mergeObservation(prior, {
        ...prior,
        ...(current.schemaVersion === 2 && !prior.checks
          ? { checks: checks("not_probed_in_version_two") }
          : {}),
      }, current.schemaVersion);
    });
    if (entries.every(Boolean)) results.push(...entries);
  }
  return { ...current, results };
}

function boundedBody(response) {
  if (!response.body) throw new Error("empty_response");
  let bytes = 0;
  return response.body.pipeThrough(new TransformStream({
    transform(chunk, controller) {
      bytes += chunk.byteLength;
      if (bytes > maxBodyBytes) throw new Error("response_too_large");
      controller.enqueue(chunk);
    },
  }));
}

async function readJson(response) {
  const reader = boundedBody(response).getReader();
  const chunks = [];
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      chunks.push(value);
    }
    return JSON.parse(Buffer.concat(chunks).toString("utf8"));
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

export function payload(model, surface, { stream = false, tools = false, reasoningEffort } = {}) {
  const content = tools ? "Call probe_echo with value OK, then reply with its result."
    : stream ? "Reply with the numbers 1 through 20 separated by spaces, and nothing else."
      : "Reply OK.";
  const message = { role: "user", content };
  const maxTokens = tools ? 256 : stream ? 64 : 16;
  const body = surface === "responses"
    ? { model, input: [message], max_output_tokens: maxTokens, stream, store: false }
    : { model, messages: [message], max_tokens: maxTokens, stream };
  if (!tools) return body;
  const definition = { name: toolName, description: "Echo the supplied value.", parameters: toolParameters, strict: false };
  if (surface === "messages") {
    body.tools = [{ name: toolName, description: definition.description, input_schema: toolParameters }];
    body.tool_choice = { type: "auto" };
  } else {
    body.tools = [surface === "responses" ? { type: "function", ...definition } : { type: "function", function: definition }];
    body.tool_choice = "auto";
    body.parallel_tool_calls = true;
    if (surface === "responses") {
      body.include = ["reasoning.encrypted_content"];
      if (reasoningEffort) body.reasoning = { effort: reasoningEffort };
    }
  }
  return body;
}

export function toolResultPayload(model, surface, body, reasoningEffort) {
  const request = payload(model, surface, { stream: true, tools: true, reasoningEffort });
  const message = { role: "user", content: "Call probe_echo with value OK, then reply with its result." };
  if (surface === "responses") {
    const call = body.output.find(item => item?.type === "function_call");
    if (!call) throw new Error("missing_tool_call");
    request.input = [message, ...body.output, { type: "function_call_output", call_id: call.call_id, output: "OK" }];
    request.tool_choice = "auto";
  } else if (surface === "messages") {
    const call = body.content.find(item => item?.type === "tool_use");
    if (!call) throw new Error("missing_tool_call");
    request.messages = [message, { role: "assistant", content: body.content }, {
      role: "user", content: [{ type: "tool_result", tool_use_id: call.id, content: "OK" }],
    }];
    request.tool_choice = { type: "auto" };
  } else {
    const assistant = body.choices[0].message;
    request.messages = [message, assistant, { role: "tool", tool_call_id: assistant.tool_calls[0].id, content: "OK" }];
    request.tool_choice = "auto";
  }
  return request;
}

function validCall(call, surface) {
  try {
    const name = surface === "chat/completions" ? call.function.name : call.name;
    const args = surface === "messages" ? call.input : JSON.parse(surface === "responses" ? call.arguments : call.function.arguments);
    const id = surface === "responses" ? call.call_id : call.id;
    return name === toolName && typeof id === "string" && id.length > 0 &&
      args?.value === "OK" && Object.keys(args).length === 1;
  } catch {
    return false;
  }
}

async function readStream(response) {
  if (response.headers.get("content-type")?.split(";")[0].trim() !== "text/event-stream") {
    throw new Error("expected_event_stream");
  }
  const reader = boundedBody(response).pipeThrough(new TextDecoderStream())
    .pipeThrough(new EventSourceParserStream({ onError: "terminate", maxBufferSize: maxBodyBytes })).getReader();
  const events = [];
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      const data = value.data === "[DONE]" ? { type: "done" } : JSON.parse(value.data);
      if (!data || typeof data !== "object") throw new Error("invalid_stream_event");
      events.push(data);
      if (["done", "message_stop", "response.completed", "response.failed", "response.incomplete", "error"].includes(data.type)) break;
    }
    return events;
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

/** Accumulate documented API events; SSE framing is handled by eventsource-parser. */
export function summarizeStream(surface, events) {
  let body;
  let terminal = false;
  let delta = false;
  const responseCalls = new Map();
  const validIndex = index => Number.isInteger(index) && index >= 0 && index < 128;
  if (events.some(event => event.error || ["error", "response.failed", "response.incomplete"].includes(event.type))) {
    return { streaming: unknown("stream_error_or_incomplete") };
  }
  if (surface === "responses") {
    body = events.find(event => event.type === "response.completed")?.response;
    terminal = body?.status === "completed" && body?.object === "response" && Array.isArray(body.output);
    delta = events.some(event => ["response.output_text.delta", "response.function_call_arguments.delta"].includes(event.type) && typeof event.delta === "string" && event.delta.length > 0);
    for (const event of events) {
      if (event.type === "response.output_item.added" && event.item?.type === "function_call") {
        responseCalls.set(event.output_index, { arguments: event.item.arguments ?? "", done: false });
      }
      if (event.type === "response.function_call_arguments.delta") {
        const call = responseCalls.get(event.output_index);
        if (!call || typeof event.delta !== "string") return { streaming: unknown("invalid_event_sequence") };
        call.arguments += event.delta;
      }
      if (event.type === "response.output_item.done" && event.item?.type === "function_call") {
        const call = responseCalls.get(event.output_index);
        if (call) call.done = call.arguments === event.item.arguments;
      }
    }
  } else if (surface === "messages") {
    body = events.find(event => event.type === "message_start")?.message;
    if (body) {
      body = { ...body, content: [] };
      const partial = new Map();
      for (const event of events) {
        if (["content_block_start", "content_block_delta"].includes(event.type) && !validIndex(event.index)) {
          return { streaming: unknown("invalid_event_sequence") };
        }
        if (event.type === "content_block_start") body.content[event.index] = { ...event.content_block };
        if (event.type === "content_block_delta") {
          const block = body.content[event.index];
          if (!block) return { streaming: unknown("invalid_event_sequence") };
          if (event.delta?.type === "text_delta") { block.text += event.delta.text; delta = true; }
          if (event.delta?.type === "input_json_delta") { partial.set(event.index, (partial.get(event.index) ?? "") + event.delta.partial_json); delta = true; }
          if (event.delta?.type === "thinking_delta") block.thinking += event.delta.thinking;
          if (event.delta?.type === "signature_delta") block.signature = (block.signature ?? "") + event.delta.signature;
        }
        if (event.type === "message_delta") body.stop_reason = event.delta?.stop_reason;
      }
      try { for (const [index, text] of partial) body.content[index].input = JSON.parse(text); }
      catch { return { streaming: unknown("invalid_tool_arguments") }; }
    }
    terminal = events.some(event => event.type === "message_stop") && body?.role === "assistant" && ["end_turn", "tool_use"].includes(body.stop_reason);
  } else {
    const message = { role: "assistant", content: "", tool_calls: [] };
    let finish;
    for (const event of events) {
      const choice = event.choices?.find(choice => choice.index === 0);
      if (!choice) continue;
      if (choice.delta?.content) { message.content += choice.delta.content; delta = true; }
      for (const call of choice.delta?.tool_calls ?? []) {
        if (!validIndex(call.index)) return { streaming: unknown("invalid_event_sequence") };
        const current = message.tool_calls[call.index] ?? { id: "", type: "function", function: { name: "", arguments: "" } };
        current.id += call.id ?? "";
        current.function.name += call.function?.name ?? "";
        current.function.arguments += call.function?.arguments ?? "";
        message.tool_calls[call.index] = current;
        if (call.function?.arguments) delta = true;
      }
      if (choice.finish_reason) finish = choice.finish_reason;
    }
    body = { choices: [{ message }] };
    terminal = events.some(event => event.type === "done") && ["stop", "tool_calls"].includes(finish);
  }
  if (!terminal || !delta) return { streaming: unknown("missing_stream_delta_or_completion") };
  const calls = surface === "responses" ? body.output.filter(item => item.type === "function_call")
    : surface === "messages" ? body.content.filter(item => item.type === "tool_use") : body.choices[0].message.tool_calls;
  const validResponseDeltas = surface !== "responses" || body.output.every((item, index) => item.type !== "function_call" || (
    responseCalls.get(index)?.done && responseCalls.get(index).arguments === item.arguments
  ));
  const tools = calls.length === 1 && validCall(calls[0], surface) && validResponseDeltas
    ? supported("valid_streamed_tool_call") : unknown("required_tool_call_missing_or_invalid");
  return { body, streaming: supported("valid_event_stream"), tools };
}

function hasText(surface, body) {
  return surface === "responses"
    ? body?.output?.some(item => item?.type === "message" && item.content?.some(part => part?.type === "output_text" && part.text?.trim()))
    : surface === "messages" ? body?.content?.some(part => part?.type === "text" && part.text?.trim())
      : !!body?.choices?.[0]?.message?.content?.trim();
}

export function classify(surface, status, body) {
  if (status === 401 || status === 403) {
    return { status: "inconclusive", reason: "authentication_or_permission", stop: true };
  }
  if (status === 429) {
    return { status: "inconclusive", reason: "rate_or_quota_limit" };
  }
  if (status === 405 || status === 501 ||
      ([400, 404, 422].includes(status) && unsupportedCodes.has(body?.error?.code))) {
    return { status: "unavailable", reason: "unsupported_endpoint_or_model" };
  }
  if ([408, 425].includes(status) || status >= 500) {
    return { status: "inconclusive", reason: "transient_server_error" };
  }
  if (status < 200 || status >= 300) {
    return { status: "inconclusive", reason: "request_rejected" };
  }
  const valid = !body?.error && (
    surface === "responses"
      ? body?.object === "response" && Array.isArray(body.output) &&
        ["completed", "incomplete"].includes(body.status)
      : surface === "messages"
        ? body?.type === "message" && body.role === "assistant" && Array.isArray(body.content)
        : Array.isArray(body?.choices) && body.choices.some(choice => choice?.message?.role === "assistant")
  );
  return valid
    ? { status: "supported", reason: "valid_response" }
    : { status: "inconclusive", reason: "unexpected_response" };
}

export async function probe({ endpoint, key, modelIds = [], surfaceNames = surfaces, concurrency = 1, timeout = 30000, basic = false, reasoningEffort, previousInventory, fetchImpl = fetch }) {
  const base = new URL(endpoint);
  if (base.protocol !== "https:" || base.username || base.password || base.search || base.hash) {
    throw new Error("Use an HTTPS endpoint without credentials, query, or fragment.");
  }
  if (!key?.trim()) throw new Error("Set the API key environment variable before probing.");
  if (!surfaceNames.length || surfaceNames.some(surface => !surfaces.includes(surface))) {
    throw new Error("Surface must be chat/completions, responses, or messages.");
  }
  if (!Number.isInteger(concurrency) || concurrency < 1 || concurrency > 4 ||
      !Number.isInteger(timeout) || timeout < 1000 || timeout > 60000) {
    throw new Error("Concurrency must be 1-4 and timeout must be 1000-60000 milliseconds.");
  }
  if (reasoningEffort && (!["low", "medium", "high"].includes(reasoningEffort) || basic)) {
    throw new Error("Reasoning effort must be low, medium, or high, and requires full probes.");
  }
  const root = base.href.replace(/\/$/, "").replace(/\/v1$/, "");
  const headers = { authorization: `Bearer ${key}`, "content-type": "application/json" };
  let catalog;
  try {
    const response = await fetchImpl(`${root}/v1/models`, {
      headers, redirect: "error", signal: AbortSignal.timeout(timeout),
    });
    if (!response.ok) throw new Error(`Model catalog returned HTTP ${response.status}.`);
    catalog = await readJson(response);
  } catch (error) {
    // Network exceptions may contain request details. Only our status error is safe to report.
    throw new Error(error.message.startsWith("Model catalog returned HTTP ")
      ? error.message : "Cannot read the model catalog; check connectivity and the endpoint.");
  }
  if (!Array.isArray(catalog?.data) || catalog.data.length === 0 ||
      catalog.data.some(model => typeof model?.id !== "string" || !model.id.trim())) {
    throw new Error("The model catalog must contain a nonempty data array of model IDs.");
  }
  const ids = [...new Set(catalog.data.map(model => model.id))];
  if (modelIds.some(id => !ids.includes(id))) throw new Error("A requested model is absent from the catalog.");
  const jobs = (modelIds.length ? [...new Set(modelIds)] : ids)
    .flatMap(model => [...new Set(surfaceNames)].map(surface => ({ model, surface })));
  const results = new Array(jobs.length);
  let cursor = 0;
  let stopped = false;
  async function request(model, surface, body) {
    if (stopped) return { result: unknown("not_probed_after_authentication_error") };
    try {
      const response = await fetchImpl(`${root}/v1/${surface}`, {
        method: "POST", redirect: "error", signal: AbortSignal.timeout(timeout),
        headers: surface === "messages" ? { ...headers, "anthropic-version": "2023-06-01" } : headers,
        body: JSON.stringify(body),
      });
      const httpStatus = response.status;
      if (body.stream && response.ok) {
        const summary = summarizeStream(surface, await readStream(response));
        return { ...summary, result: summary.streaming, httpStatus };
      }
      let data;
      try { data = await readJson(response); } catch { /* Preserve the HTTP failure classification. */ }
      const { stop, ...result } = classify(surface, httpStatus, data);
      if (stop) stopped = true;
      return { result, httpStatus };
    } catch {
      return { result: unknown("network_timeout_or_invalid_response") };
    }
  }
  async function worker() {
    while (cursor < jobs.length) {
      const index = cursor++;
      const { model, surface } = jobs[index];
      const { result, httpStatus } = await request(model, surface, payload(model, surface));
      let capabilityChecks;
      if (!basic) {
        capabilityChecks = checks("not_probed");
        if (result.status === "supported") {
          const toolStream = await request(model, surface, payload(model, surface, {
            stream: true, tools: true, reasoningEffort,
          }));
          capabilityChecks.streaming = withHttpStatus(toolStream.result, toolStream.httpStatus);
          capabilityChecks.tools = withHttpStatus(toolStream.tools ?? toolStream.result, toolStream.httpStatus);
          if (!stopped && capabilityChecks.streaming.status !== "supported") {
            const textStream = await request(model, surface, payload(model, surface, { stream: true }));
            capabilityChecks.streaming = withHttpStatus(textStream.result, textStream.httpStatus);
          }
          if (capabilityChecks.streaming.status === "supported" && capabilityChecks.tools.status === "supported") {
            try {
              const resumed = await request(model, surface, toolResultPayload(model, surface, toolStream.body, reasoningEffort));
              capabilityChecks.toolResult = withHttpStatus(
                resumed.result.status === "supported" && hasText(surface, resumed.body)
                  ? supported("valid_tool_result_response")
                  : resumed.result.status === "supported" ? unknown("no_tool_result_text") : resumed.result,
                resumed.httpStatus,
              );
            } catch {
              capabilityChecks.toolResult = unknown("invalid_tool_call_shape");
            }
          }
        }
      }
      results[index] = { model, endpoint: `/v1/${surface}`, ...result, ...(capabilityChecks ? { checks: capabilityChecks } : {}), ...(httpStatus ? { httpStatus } : {}) };
    }
  }
  await Promise.all(Array.from({ length: concurrency }, worker));
  const report = { schemaVersion: basic ? 1 : 2, endpoint: root, checkedAt: new Date().toISOString(), ...(reasoningEffort ? { reasoningEffort } : {}), results };
  return previousInventory ? mergeInventory(previousInventory, report, ids) : report;
}

async function main() {
  const { values } = parseArgs({ options: {
    endpoint: { type: "string" }, "key-env": { type: "string", default: "PAP_PROBE_API_KEY" },
    model: { type: "string", multiple: true }, json: { type: "boolean" },
    surface: { type: "string", multiple: true },
    previous: { type: "string" },
    basic: { type: "boolean" }, "reasoning-effort": { type: "string" },
    concurrency: { type: "string", default: "1" }, timeout: { type: "string", default: "30000" },
    help: { type: "boolean" },
  } });
  if (values.help) {
    console.log("Usage: node scripts/probe-model-endpoints.mjs --endpoint https://tee.redpill.ai [--key-env PAP_PROBE_API_KEY] [--model ID] [--surface responses] [--previous PATH] [--basic] [--reasoning-effort low|medium|high] [--json]\nChecks text, streaming, streamed tool calls, and tool results (up to 4 inferences per model and endpoint). --previous retains earlier conclusive evidence when a refresh is inconclusive. --basic checks text only. Requests may be billed. No retries.");
    return;
  }
  if (!values.endpoint) throw new Error("Provide --endpoint. Use --help for usage.");
  let previousInventory;
  if (values.previous) {
    try { previousInventory = JSON.parse(await readFile(values.previous, "utf8")); }
    catch { throw new Error("Cannot read the previous endpoint inventory."); }
  }
  const report = await probe({
    endpoint: values.endpoint, key: process.env[values["key-env"]], modelIds: values.model,
    surfaceNames: values.surface,
    basic: values.basic, reasoningEffort: values["reasoning-effort"],
    previousInventory,
    concurrency: Number(values.concurrency), timeout: Number(values.timeout),
  });
  if (values.json) console.log(JSON.stringify(report, null, 2));
  else {
    console.log(`Endpoint capabilities: ${report.endpoint}`);
    console.table(report.results);
  }
  if (report.results.some(result => result.status === "inconclusive" || Object.values(result.checks ?? {}).some(check => check.status === "inconclusive"))) process.exitCode = 2;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch(error => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
