import { parseArgs } from "node:util";
import { pathToFileURL } from "node:url";

const surfaces = ["chat/completions", "responses", "messages"];
const unsupportedCodes = new Set([
  "unsupported_model", "model_not_supported", "model_not_found",
  "unsupported_endpoint", "endpoint_not_supported",
]);
const maxBodyBytes = 1024 * 1024;

async function readJson(response) {
  if (!response.body) throw new Error("empty_response");
  const reader = response.body.getReader();
  const chunks = [];
  let length = 0;
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > maxBodyBytes) throw new Error("response_too_large");
      chunks.push(value);
    }
    return JSON.parse(Buffer.concat(chunks).toString("utf8"));
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

function payload(model, surface) {
  const message = { role: "user", content: "Reply OK." };
  return surface === "responses"
    ? { model, input: [message], max_output_tokens: 16, stream: false }
    : { model, messages: [message], max_tokens: 16, stream: false };
}

export function classify(surface, status, body) {
  if (status === 401 || status === 403) {
    return { status: "inconclusive", reason: "authentication_or_permission", stop: true };
  }
  if (status === 429) {
    return { status: "inconclusive", reason: "rate_or_quota_limit", stop: true };
  }
  if (status === 405 || status === 501 ||
      ([400, 404, 422].includes(status) && unsupportedCodes.has(body?.error?.code))) {
    return { status: "unavailable", reason: "unsupported_endpoint_or_model" };
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

export async function probe({ endpoint, key, modelIds = [], surfaceNames = surfaces, concurrency = 2, timeout = 20000 }) {
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
  const root = base.href.replace(/\/$/, "").replace(/\/v1$/, "");
  const headers = { authorization: `Bearer ${key}`, "content-type": "application/json" };
  let catalog;
  try {
    const response = await fetch(`${root}/v1/models`, {
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
  async function worker() {
    while (cursor < jobs.length) {
      const index = cursor++;
      const { model, surface } = jobs[index];
      let result = { status: "inconclusive", reason: "not_probed_after_auth_or_limit_error" };
      let httpStatus;
      if (!stopped) {
        try {
          const response = await fetch(`${root}/v1/${surface}`, {
            method: "POST", redirect: "error", signal: AbortSignal.timeout(timeout),
            headers: surface === "messages" ? { ...headers, "anthropic-version": "2023-06-01" } : headers,
            body: JSON.stringify(payload(model, surface)),
          });
          httpStatus = response.status;
          let body;
          try { body = await readJson(response); } catch { /* Classification preserves HTTP failures. */ }
          result = classify(surface, httpStatus, body);
          if (result.stop) stopped = true;
        } catch {
          result = { status: "inconclusive", reason: "network_or_timeout" };
        }
      }
      const { stop: _stop, ...publicResult } = result;
      results[index] = { model, endpoint: `/v1/${surface}`, ...publicResult, ...(httpStatus ? { httpStatus } : {}) };
    }
  }
  await Promise.all(Array.from({ length: concurrency }, worker));
  return { schemaVersion: 1, endpoint: root, checkedAt: new Date().toISOString(), results };
}

async function main() {
  const { values } = parseArgs({ options: {
    endpoint: { type: "string" }, "key-env": { type: "string", default: "PAP_PROBE_API_KEY" },
    model: { type: "string", multiple: true }, json: { type: "boolean" },
    surface: { type: "string", multiple: true },
    concurrency: { type: "string", default: "2" }, timeout: { type: "string", default: "20000" },
    help: { type: "boolean" },
  } });
  if (values.help) {
    console.log("Usage: node scripts/probe-model-endpoints.mjs --endpoint https://tee.redpill.ai [--key-env PAP_PROBE_API_KEY] [--model ID] [--surface responses] [--json]\nSends one small inference per model and endpoint. Requests may be billed. No retries.");
    return;
  }
  if (!values.endpoint) throw new Error("Provide --endpoint. Use --help for usage.");
  const report = await probe({
    endpoint: values.endpoint, key: process.env[values["key-env"]], modelIds: values.model,
    surfaceNames: values.surface,
    concurrency: Number(values.concurrency), timeout: Number(values.timeout),
  });
  if (values.json) console.log(JSON.stringify(report, null, 2));
  else {
    console.log(`Endpoint capabilities: ${report.endpoint}`);
    console.table(report.results);
  }
  if (report.results.some(result => result.status === "inconclusive")) process.exitCode = 2;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch(error => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
