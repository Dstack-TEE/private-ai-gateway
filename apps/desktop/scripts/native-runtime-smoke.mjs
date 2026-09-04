import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import readline from "node:readline";

const executable = process.argv[2];
if (!executable) throw new Error("Usage: node native-runtime-smoke.mjs <runtime executable>");

const fixtures = JSON.parse(
  await readFile(new URL("../native/protocol-fixtures/v1.json", import.meta.url), "utf8"),
);
for (const name of ["success", "failure", "event"]) {
  decodeEnvelope(JSON.stringify(fixtures[name]));
}

const home = await mkdtemp(path.join(os.tmpdir(), "private-ai-gateway-smoke-"));
const child = spawn(path.resolve(executable), [], {
  cwd: path.dirname(path.resolve(executable)),
  env: {
    ...process.env,
    PRIVATE_AI_GATEWAY_HOME: home,
    HOME: home,
    USERPROFILE: home,
  },
  stdio: ["pipe", "pipe", "inherit"],
});

const pending = new Map();
let nextId = 1;
let fatal;
const lines = readline.createInterface({ input: child.stdout });
lines.on("line", (line) => {
  let message;
  try {
    message = decodeEnvelope(line);
  } catch {
    fatal = new Error("Runtime emitted an invalid protocol envelope");
    return;
  }
  if (message.event) return;
  const entry = pending.get(message.id);
  if (!entry) return;
  pending.delete(message.id);
  clearTimeout(entry.timer);
  entry.resolve(message);
});

try {
  const state = await sendEnvelope({ ...fixtures.request, id: String(nextId++) });
  assertSuccess(state, "getState");
  if (state.schemaVersion !== 1 || state.result?.status !== "stopped") {
    throw new Error("Runtime returned an incompatible initial state");
  }
  if (!Array.isArray(state.result.profiles) || !state.result.localApi) {
    throw new Error("Runtime state is missing profile or Local API contracts");
  }
  const serialized = JSON.stringify(state.result);
  if (/\b(?:sk-|pag_)[A-Za-z0-9_-]{8,}/.test(serialized)) {
    throw new Error("Runtime state exposed credential material");
  }

  const agents = await request("listAgents", {});
  assertSuccess(agents, "listAgents");
  if (!Array.isArray(agents.result) || agents.result.length !== 5) {
    throw new Error("Runtime did not expose the five supported agents");
  }

  const incompatible = await rawRequest(99, "getState", {});
  if (incompatible.error?.code !== "unsupported_schema") {
    throw new Error("Runtime did not reject an unsupported protocol schema");
  }

  const shutdown = await request("shutdown", {});
  assertSuccess(shutdown, "shutdown");
  if (shutdown.result !== null) throw new Error("Shutdown must return JSON null");

  const exitCode = await waitForExit(child, 10_000);
  if (exitCode !== 0) throw new Error(`Runtime exited with status ${exitCode}`);
  if (fatal) throw fatal;
  console.log("Native runtime protocol smoke passed");
} finally {
  lines.close();
  if (child.exitCode === null) child.kill();
  await rm(home, { recursive: true, force: true });
}

function request(method, params) {
  return rawRequest(1, method, params);
}

function rawRequest(schemaVersion, method, params) {
  return sendEnvelope({ schemaVersion, id: String(nextId++), method, params });
}

function sendEnvelope(envelope) {
  const { id } = envelope;
  const message = JSON.stringify(envelope);
  if (Buffer.byteLength(message) > 1024 * 1024) throw new Error("Smoke request is too large");
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      pending.delete(id);
      reject(new Error(`Timed out waiting for ${method}`));
    }, 15_000);
    pending.set(id, { resolve, reject, timer });
    child.stdin.write(`${message}\n`, (error) => {
      if (!error) return;
      clearTimeout(timer);
      pending.delete(id);
      reject(error);
    });
  });
}

function decodeEnvelope(line) {
  if (Buffer.byteLength(line) > 1024 * 1024) {
    throw new Error("Runtime response is too large");
  }
  const message = JSON.parse(line);
  if (!message || typeof message !== "object" || message.schemaVersion !== 1) {
    throw new Error("Unsupported runtime protocol schema");
  }
  if (typeof message.event === "string") {
    if (!("payload" in message) || "id" in message) throw new Error("Invalid runtime event");
    return message;
  }
  if (typeof message.id !== "string") throw new Error("Runtime response has no id");
  const outcomes = Number("result" in message) + Number("error" in message);
  if (outcomes !== 1) throw new Error("Runtime response must have exactly one outcome");
  return message;
}

function assertSuccess(response, method) {
  if (response.error) throw new Error(`${method} failed: ${response.error.message}`);
  if (!("result" in response)) throw new Error(`${method} returned no result`);
}

function waitForExit(process, timeoutMs) {
  if (process.exitCode !== null) return Promise.resolve(process.exitCode);
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("Runtime did not exit after shutdown")), timeoutMs);
    process.once("exit", (code) => {
      clearTimeout(timer);
      resolve(code);
    });
  });
}
