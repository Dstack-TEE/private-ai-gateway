import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import os from "node:os";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { promisify } from "node:util";

const [application, driverPath, nativeDriver, evidenceDirectory] = process.argv.slice(2);
assert.ok([application, driverPath, nativeDriver, evidenceDirectory].every((value) => value && path.isAbsolute(value)),
  "Supply absolute installed app, tauri-driver, native driver, and evidence paths");
const home = await mkdtemp(path.join(os.tmpdir(), "tauri-installed-"));
const data = path.join(home, ".private-ai-gateway");
const env = { ...process.env, PRIVATE_AI_GATEWAY_HOME: home };
const xdgDirectories = process.platform === "linux" ? {
  XDG_DATA_HOME: path.join(home, "data"), XDG_CONFIG_HOME: path.join(home, "config"),
  XDG_CACHE_HOME: path.join(home, "cache"), XDG_STATE_HOME: path.join(home, "state"),
} : {};
Object.assign(env, xdgDirectories);
let driver;
let driverExit;
let driverError;
let driverExited;
let session;
let base;

async function freePort() {
  const socket = createServer();
  socket.listen(0, "127.0.0.1");
  await once(socket, "listening");
  const address = socket.address();
  assert.ok(address && typeof address !== "string");
  await new Promise((resolve, reject) => socket.close((error) => error ? reject(error) : resolve()));
  return address.port;
}

async function request(method, route, body) {
  const response = await fetch(`${base}${route}`, { method,
    headers: { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(30_000) });
  const result = await response.json();
  assert.ok(response.ok, `WebDriver ${route}: ${JSON.stringify(result.value)}`);
  return result.value;
}

async function until(check) {
  const deadline = Date.now() + 30_000;
  let lastError;
  while (Date.now() < deadline) {
    if (driverError) throw driverError;
    if (driverExited) throw new Error(`tauri-driver exited with status ${driver.exitCode}`);
    try { if (await check()) return; } catch (error) { lastError = error; }
    await delay(250);
  }
  throw new Error("Installed app did not become ready", { cause: lastError });
}

const execute = (script, args = []) => request("POST", `/session/${session}/execute/sync`, { script, args });
async function invoke(command, args = {}) {
  const result = await request("POST", `/session/${session}/execute/async`, {
    script: `const done = arguments[arguments.length - 1];
      window.__TAURI_INTERNALS__.invoke(arguments[0], arguments[1]).then(
        value => done({ value }), error => done({ error: String(error) }));`, args: [command, args],
  });
  if (result.error) throw new Error(result.error);
  return result.value;
}

async function launch() {
  const port = await freePort();
  const nativePort = await freePort();
  assert.notEqual(port, nativePort, "WebDriver ports must be distinct");
  base = `http://127.0.0.1:${port}`;
  driver = spawn(driverPath, ["--port", String(port), "--native-port", String(nativePort), "--native-driver", nativeDriver],
    { env, detached: process.platform !== "win32", stdio: ["ignore", "inherit", "inherit"] });
  driverError = undefined;
  driverExited = false;
  driverExit = new Promise((resolve) => {
    driver.on("error", (error) => {
      driverError = error;
      // A failed spawn has no process to reap. Other errors do not mean exit.
      if (!driver.pid) { driverExited = true; resolve(); }
    });
    driver.once("exit", () => { driverExited = true; resolve(); });
  });
  await until(async () => {
    assert.equal(driver.exitCode, null, "tauri-driver exited");
    return (await request("GET", "/status")).ready;
  });
  const created = await request("POST", "/session", { capabilities: { alwaysMatch: {
    "tauri:options": { application },
  } } });
  session = created.sessionId;
  assert.ok(session);
  if (process.platform === "win32") {
    const version = created.capabilities.browserVersion;
    assert.equal(typeof version, "string", "WebDriver did not report the WebView2 version");
    assert.equal(version.split(".").slice(0, 3).join("."), process.env.TAURI_SMOKE_EDGE_BUILD,
      "The actual WebView2 runtime does not match the selected EdgeDriver build");
    console.log(`Installed WebView2 runtime: ${version}`);
  }
  await until(() => execute("return !!window.__TAURI_INTERNALS__ && !!document.querySelector('#nav-overview')"));
}

async function shutdown() {
  try {
    if (session) await request("DELETE", `/session/${session}`);
  } finally {
    session = undefined;
    if (driver?.pid) {
      // CloseRequested deliberately hides this tray app. End the owned driver
      // process tree too; session deletion alone is not an application quit.
      if (process.platform === "win32") {
        if (!driverExited) {
          try {
            await promisify(execFile)(path.join(process.env.SystemRoot, "System32", "taskkill.exe"),
              ["/PID", String(driver.pid), "/T", "/F"], { timeout: 5000 });
          } catch (error) {
            // tauri-driver owns a kill-on-close Windows Job, including children.
            // Kill the driver even when taskkill itself fails or times out.
            if (!driverExited && !driver.kill("SIGKILL")) throw error;
          }
        }
      } else {
        try { process.kill(-driver.pid, "SIGKILL"); } catch (error) { if (error.code !== "ESRCH") throw error; }
      }
      let timer;
      try {
        await Promise.race([driverExit, new Promise((_, reject) => {
          timer = setTimeout(() => reject(new Error("Timed out reaping the owned driver tree")), 5000);
        })]);
      } finally {
        clearTimeout(timer);
      }
    }
    driver = undefined;
  }
}

try {
  for (const directory of Object.values(xdgDirectories)) {
    await mkdir(directory, { recursive: true, mode: 0o700 });
  }
  await mkdir(data, { recursive: true, mode: 0o700 });
  await mkdir(evidenceDirectory, { recursive: true });
  const config = { listenAddress: "127.0.0.1", allowNetworkAccess: false, port: await freePort() };
  await writeFile(path.join(data, "local-api.json"), JSON.stringify(config), { mode: 0o600 });
  await launch();
  const state = await invoke("get_gateway_state");
  assert.equal(state.status, "stopped");
  assert.equal(state.apiKeySaved, false);
  assert.ok(!state.error && !state.endpointError, JSON.stringify(state));
  assert.equal(state.proxyUrl, `http://127.0.0.1:${config.port}`);
  const agents = await invoke("list_agents");
  assert.deepEqual(agents.map((agent) => agent.id).sort(), ["claude-code", "codex", "hermes", "opencode", "pi"]);
  await assert.rejects(invoke("preview_agent_connection", { agentId: "codex", connect: true, options: {} }), /wait until it is verified/);
  assert.equal((await invoke("disconnect_all_agents")).length, 5);

  assert.deepEqual((await invoke("query_usage", { query: {} })).items, []);
  const csv = path.join(home, "usage.csv");
  assert.equal(await invoke("export_usage_csv", { query: {}, path: csv }), 0);
  assert.ok((await readFile(csv, "utf8")).includes("agent"));
  const oldToken = await invoke("get_client_key");
  const token = await invoke("rotate_client_key");
  assert.notEqual(token, oldToken);
  for (const [credential, expected] of [[oldToken, 401], [token, 503]]) {
    const response = await fetch(`${state.proxyUrl}/v1/models`, {
      headers: { Authorization: `Bearer ${credential}` }, signal: AbortSignal.timeout(5000),
    });
    assert.equal(response.status, expected);
    await response.arrayBuffer();
  }
  await execute("document.querySelector('#nav-settings').click()");
  await until(() => execute("return document.querySelector('#nav-settings').getAttribute('aria-current') === 'page'"));
  const screenshot = await request("GET", `/session/${session}/screenshot`);
  await writeFile(path.join(evidenceDirectory, "installed-settings.png"), Buffer.from(screenshot, "base64"));
  config.port = await freePort();
  assert.equal((await invoke("save_local_api_config", { config })).localApi.port, config.port);
  await shutdown();
  await launch();
  assert.equal((await invoke("get_gateway_state")).localApi.port, config.port);
  assert.equal(await invoke("get_client_key"), token);
  console.log("Installed WebView: real React/IPC, five-agent discovery, unverified-connect rejection, empty restore/export, local token rotation/revocation, settings persistence and restart passed");
  console.log("Not covered: verified connect/inference, native tray interaction, login registration or graceful Quit");
} catch (error) {
  if (session) {
    try {
      const screenshot = await request("GET", `/session/${session}/screenshot`);
      await writeFile(path.join(evidenceDirectory, "installed-failure.png"), Buffer.from(screenshot, "base64"));
    } catch (captureError) {
      console.error("Could not capture the failed WebView:", captureError.message);
    }
  }
  throw error;
} finally {
  try { await shutdown(); } finally { await rm(home, { recursive: true, force: true }); }
}
