// Live end-to-end test of the installed Private AI Proxy with every supported
// coding agent. run.sh runs it as the test user inside the prepared container
// and passes the RedPill API key on stdin, which only `pap profiles add` reads.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { after, before, test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";

import { parse as parseToml } from "smol-toml";
import { parse as parseYaml } from "yaml";

const MODEL = "z-ai/glm-5.3-flash";
const PROMPT = "Reply with exactly: PAP-OK";
const REPLY = "PAP-OK";
const HOME = os.homedir();
const TOKENS = path.join(HOME, ".local/share/org.dstack.private-ai-proxy/agent-tokens");

// `files` are the configuration files a connection edits; `prompt` is the
// agent's one-shot command, which must use the default model connect selects;
// `selfWritten` are top-level keys the agent adds to those files by itself.
const AGENTS = [
  {
    id: "claude-code",
    surface: "messages",
    files: [".claude/settings.json"],
    prompt: ["claude", "-p", PROMPT],
  },
  {
    id: "codex",
    surface: "responses",
    files: [".codex/config.toml"],
    prompt: ["codex", "exec", "--skip-git-repo-check", PROMPT],
  },
  {
    id: "opencode",
    surface: "chat",
    files: [".config/opencode/opencode.json"],
    prompt: ["opencode", "run", PROMPT],
    selfWritten: ["$schema"],
  },
  {
    id: "pi",
    surface: "chat",
    files: [".pi/agent/models.json", ".pi/agent/settings.json"],
    prompt: ["pi", "-p", "--no-session", PROMPT],
  },
  {
    id: "oh-my-pi",
    surface: "chat",
    files: [".omp/agent/models.yml", ".omp/agent/config.yml"],
    prompt: ["omp", "-p", PROMPT],
  },
  {
    id: "openclaw",
    surface: "chat",
    files: [".openclaw/openclaw.json"],
    prompt: ["openclaw", "agent", "--local", "--agent", "main", "--session-id", "pap-e2e", "-m", PROMPT],
  },
  {
    id: "hermes",
    surface: "chat",
    files: [".hermes/config.yaml"],
    prompt: ["hermes", "chat", "-q", PROMPT, "--oneshot", "-Q"],
  },
];

const INFERENCE = {
  chat: ["/v1/chat/completions", { model: MODEL, max_tokens: 16, messages: [{ role: "user", content: PROMPT }] }],
  messages: ["/v1/messages", { model: MODEL, max_tokens: 16, messages: [{ role: "user", content: PROMPT }] }],
  responses: ["/v1/responses", { model: MODEL, max_output_tokens: 16, input: PROMPT }],
};

// Provider definitions a disconnect keeps by design (provider_namespace in
// agent-bridge/src/agents/projection.rs); everything else must be restored.
const RETAINED = [
  ["model_providers", "private_ai_proxy"],
  ["provider", "private-ai-proxy"],
  ["providers", "private-ai-proxy"],
  ["models", "providers", "private-ai-proxy"],
  ["secrets", "providers", "private-ai-proxy"],
];

const apiKey = readFileSync(0, "utf8").trim();
const STEPS = ["detected", "reply", "usage", "restored", "revoked"];
const results = new Map(AGENTS.map((agent) => [agent.id, {}]));
const session = {};
let localApi;

function run(command, args, { input, timeout = 120_000 } = {}) {
  const result = spawnSync(command, args, {
    input,
    stdio: [input === undefined ? "ignore" : "pipe", "pipe", "pipe"],
    encoding: "utf8",
    timeout,
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.error) throw new Error(`${command} did not finish: ${result.error.message}`);
  return result;
}

function pap(args, options) {
  const result = run("pap", args, options);
  assert.equal(result.status, 0, `pap ${args.join(" ")} exited ${result.status}: ${result.stderr.trim()}`);
  return result.stdout;
}

const papJson = (...args) => JSON.parse(pap(["--json", ...args]));
const protection = () => papJson("status").gateway.protection.phase;
const listed = (agent) => papJson("agents", "list").find(({ id }) => id === agent.id);
const readToken = (agent) => readFileSync(path.join(TOKENS, agent.id), "utf8").trim();
const nowSeconds = () => Math.floor(Date.now() / 1000);
const tail = (text) => text.trim().split("\n").slice(-15).join("\n");

async function step(row, name, check) {
  try {
    const value = await check();
    row[name] = "ok";
    return value;
  } catch (error) {
    row[name] = "FAIL";
    throw error;
  }
}

const connect = (agent) => pap(["agents", "connect", agent.id, "--model", MODEL, "--yes"]);
const readFiles = (agent) =>
  agent.files.map((file) => {
    const absolute = path.join(HOME, file);
    return existsSync(absolute) ? readFileSync(absolute, "utf8") : null;
  });

function parseConfig(file, text) {
  if (text === null) return {};
  if (file.endsWith(".toml")) return parseToml(text);
  if (/\.ya?ml$/.test(file)) return parseYaml(text) ?? {};
  return JSON.parse(text);
}

// A parent object that only held a removed entry is equivalent to an absent one.
function prune(value) {
  if (Array.isArray(value)) return value.map(prune);
  if (value === null || typeof value !== "object" || value instanceof Date) return value;
  return Object.fromEntries(
    Object.entries(value)
      .map(([key, child]) => [key, prune(child)])
      .filter(([, child]) => !(child?.constructor === Object && Object.keys(child).length === 0)),
  );
}

// The routing and credentials a disconnect must restore: each file without
// the provider definitions it keeps and the agent's own additions.
function routing(agent) {
  return readFiles(agent).map((text, index) => {
    const config = parseConfig(agent.files[index], text);
    for (const key of agent.selfWritten ?? []) delete config[key];
    for (const keys of RETAINED) {
      const parent = keys.slice(0, -1).reduce((node, key) => node?.[key], config);
      if (parent && typeof parent === "object") delete parent[keys.at(-1)];
    }
    return prune(config);
  });
}

// Each probe opens its own connection: the Local API closes idle keep-alive
// connections, and a pooled one may already be closed when it is reused.
function localApiStatus(token, pathname, body) {
  return new Promise((resolve, reject) => {
    const request = http.request(new URL(pathname, localApi), {
      agent: false,
      method: body ? "POST" : "GET",
      headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
      timeout: 60_000,
    });
    request.on("response", (response) => {
      response.resume();
      resolve(response.statusCode);
    });
    request.on("timeout", () => request.destroy(new Error(`${pathname} timed out`)));
    request.on("error", reject);
    request.end(body && JSON.stringify(body));
  });
}

function assertReply(agent) {
  const [command, ...args] = agent.prompt;
  const { status, stdout, stderr } = run(command, args, { timeout: 240_000 });
  const replied = stdout.split("\n").some((line) => line.trim() === REPLY);
  assert.ok(replied, `${agent.id} exited ${status} without replying ${REPLY}:\n${tail(stderr)}\n${tail(stdout)}`);
}

// Receipt audits finish after delivery, so wait for every record to settle.
async function settledUsage(agent, since) {
  const deadline = Date.now() + 120_000;
  for (;;) {
    const { items } = papJson("usage", "list", "--agent", agent.id, "--since", String(since), "--limit", "100");
    if ((items.length > 0 && items.every(({ verified }) => verified != null)) || Date.now() > deadline) {
      return items;
    }
    await sleep(2_000);
  }
}

before(() => {
  assert.ok(apiKey, "Pass the RedPill API key on stdin");
  pap(["service", "start"]);
  const profile = ["--id", "redpill", "--name", "RedPill", "--provider", "redpill", "--url", "https://tee.redpill.ai"];
  pap(["profiles", "add", ...profile, "--key-stdin", "--yes"], { input: apiKey });
  pap(["start", "--yes"]);
  assert.equal(protection(), "protected");
  localApi = papJson("status").gateway.proxyUrl;
});

for (const agent of AGENTS) {
  test(agent.id, async () => {
    const row = results.get(agent.id);
    await step(row, "detected", () => assert.ok(listed(agent)?.installed, `${agent.id} is not detected`));
    const original = routing(agent);
    connect(agent);
    const token = readToken(agent);
    assert.equal(await localApiStatus(token, "/v1/models"), 200, "the new agent token is not accepted");

    const since = nowSeconds();
    await step(row, "reply", () => assertReply(agent));
    await step(row, "usage", async () => {
      const records = await settledUsage(agent, since);
      assert.ok(records.length > 0, `no usage record for ${agent.id}`);
      for (const { path: requestPath, status, verified, detail } of records) {
        assert.ok(status === 200 && verified === true, `${requestPath}: HTTP ${status}, verified ${verified}: ${detail}`);
      }
    });

    pap(["agents", "disconnect", agent.id, "--yes"]);
    await step(row, "restored", () => assert.deepEqual(routing(agent), original));
    await step(row, "revoked", async () => assert.equal(await localApiStatus(token, "/v1/models"), 401));
  });
}

const passed = () => AGENTS.filter((agent) => STEPS.every((name) => results.get(agent.id)[name] === "ok"));
const connected = new Map();

test("fails closed while verification fails and recovers", async () => {
  const agents = passed();
  assert.ok(agents.length > 0, "no agent passed its own test");
  for (const agent of agents) {
    const original = routing(agent);
    connect(agent);
    connected.set(agent.id, { original, files: readFiles(agent), token: readToken(agent) });
  }

  // An imported profile has no credential, so protection cannot verify it.
  const backup = path.join(os.tmpdir(), "pap-e2e-unverified-profile.json");
  const name = "E2E unverified";
  writeFileSync(backup, JSON.stringify({ version: 1, profiles: [{ name, provider: "custom", remoteUrl: "https://unverified.invalid" }] }));
  pap(["profiles", "import", backup, "--yes"]);
  const unverified = papJson("profiles", "list").find((profile) => profile.name === name);
  const since = nowSeconds();
  await step(session, "fail closed", async () => {
    assert.notEqual(run("pap", ["profiles", "use", unverified.id, "--yes"]).status, 0, "an unverifiable profile was accepted");
    assert.notEqual(protection(), "protected");
    for (const agent of agents) {
      const { files, token } = connected.get(agent.id);
      assert.deepEqual(readFiles(agent), files, `${agent.id} no longer points at the Local API`);
      assert.ok(listed(agent).connected, `${agent.id} is reported disconnected`);
      const [pathname, body] = INFERENCE[agent.surface];
      const status = await localApiStatus(token, pathname, body);
      assert.ok(status >= 400, `${agent.id}: ${pathname} answered HTTP ${status} while unverified`);
    }
    const { items } = papJson("usage", "list", "--since", String(since), "--limit", "100");
    const forwarded = items.filter(({ leftDevice }) => leftDevice);
    assert.equal(forwarded.length, 0, `forwarded while unverified: ${forwarded.map(({ path: p }) => p).join(", ")}`);
  });

  await step(session, "recovered", async () => {
    pap(["start", "--profile", "redpill", "--yes"]);
    assert.equal(protection(), "protected");
    for (const agent of agents) {
      const { token } = connected.get(agent.id);
      assert.equal(await localApiStatus(token, "/v1/models"), 200, `${agent.id} token not accepted after recovery`);
    }
    assertReply(agents[0]);
  });
});

test("stop restores every agent and revokes every token", async () => {
  const agents = passed().filter((agent) => connected.has(agent.id));
  assert.ok(agents.length > 0, "no agent is connected");
  await step(session, "stop restores", async () => {
    pap(["stop", "--yes"]);
    for (const agent of agents) {
      const { original, token } = connected.get(agent.id);
      assert.deepEqual(routing(agent), original, `${agent.id} was not restored`);
      assert.equal(await localApiStatus(token, "/v1/models"), 401, `${agent.id} token still accepted`);
    }
    pap(["service", "stop", "--yes"]);
    for (const agent of agents) {
      assert.deepEqual(routing(agent), connected.get(agent.id).original, `${agent.id} changed on service stop`);
    }
  });
});

after(() => {
  const rows = [["agent", ...STEPS], ...[...results].map(([id, row]) => [id, ...STEPS.map((name) => row[name] ?? "-")])];
  const widths = rows[0].map((_, column) => Math.max(...rows.map((row) => row[column].length)));
  const lines = rows.map((row) => row.map((cell, column) => cell.padEnd(widths[column])).join("  ").trimEnd());
  const phases = ["fail closed", "recovered", "stop restores"].map((name) => `${name}: ${session[name] ?? "-"}`);
  console.log(["", ...lines, "", phases.join("  "), ""].join("\n"));
});
