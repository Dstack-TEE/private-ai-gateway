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
let launchNumber = 0;
let captureDriverLog = false;
let driverLog = "";
const diagnosticEvents = [];
let previousProcesses = new Map();

function record(event, details = {}) {
  diagnosticEvents.push({ at: new Date().toISOString(), event, ...details });
}

async function sampleWindowsProcesses() {
  // No command lines, environment, window titles, or unrelated process data.
  const script = `
    $all = @(Get-CimInstance Win32_Process -Filter "Name = 'private-ai-gateway-desktop.exe' OR Name = 'msedgewebview2.exe' OR Name = 'msedgedriver.exe'")
    $selected = @($all | Where-Object { $_.ExecutablePath -eq $env:TAURI_SMOKE_APPLICATION -or $_.ProcessId -eq [int]$env:TAURI_SMOKE_DRIVER_PID })
    for ($depth = 0; $depth -lt 8; $depth++) {
      $ids = @($selected | ForEach-Object ProcessId)
      $next = @($all | Where-Object { $_.ParentProcessId -in $ids -and $_.ProcessId -notin $ids })
      if (!$next.Count) { break }
      $selected += $next
    }
    ConvertTo-Json -Compress -InputObject @($selected | Select-Object ProcessId, ParentProcessId, Name, ExecutablePath, CreationDate)
  `;
  const { stdout } = await promisify(execFile)("powershell.exe", ["-NoProfile", "-NonInteractive", "-EncodedCommand",
    Buffer.from(script, "utf16le").toString("base64")], {
    env: { ...process.env, TAURI_SMOKE_APPLICATION: application, TAURI_SMOKE_DRIVER_PID: String(driver?.pid ?? 0) },
    timeout: 5000, maxBuffer: 262144,
  });
  const processes = JSON.parse(stdout);
  const current = new Map(processes.map((process) => [`${process.ProcessId}:${process.CreationDate}`, process]));
  for (const [key, process] of current) {
    if (!previousProcesses.has(key)) record("process-first-observed", { process });
  }
  for (const [key, process] of previousProcesses) {
    if (!current.has(key)) record("process-no-longer-observed", { process });
  }
  previousProcesses = current;
  record("windows-process-snapshot", { launch: launchNumber, processes });
}

async function monitorWindowsLaunch(signal) {
  while (!signal.aborted) {
    try { await sampleWindowsProcesses(); } catch (error) {
      record("process-snapshot-error", { message: error.message });
    }
    try { await delay(1000, undefined, { signal }); } catch { break; }
  }
}

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
  const started = Date.now();
  record("webdriver-request", { method, route });
  try {
    const response = await fetch(`${base}${route}`, { method,
      headers: { "Content-Type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(30_000) });
    const result = await response.json();
    record("webdriver-response", { method, route, status: response.status, elapsedMs: Date.now() - started });
    assert.ok(response.ok, `WebDriver ${route}: ${JSON.stringify(result.value)}`);
    return result.value;
  } catch (error) {
    record("webdriver-request-error", { method, route, elapsedMs: Date.now() - started, name: error.name });
    throw new Error(`WebDriver ${method} ${route} failed after ${Date.now() - started}ms`, { cause: error });
  }
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
  launchNumber++;
  const port = await freePort();
  const nativePort = await freePort();
  assert.notEqual(port, nativePort, "WebDriver ports must be distinct");
  base = `http://127.0.0.1:${port}`;
  // Match tauri-driver 2.0.6's Windows capabilities directly so the native
  // driver can receive --verbose (the intermediary cannot forward that flag).
  // https://learn.microsoft.com/microsoft-edge/webview2/how-to/webdriver
  // https://learn.microsoft.com/microsoft-edge/webdriver/capabilities-edge-options
  const windows = process.platform === "win32";
  const executable = windows ? nativeDriver : driverPath;
  const args = windows ? [`--port=${port}`, "--host=127.0.0.1", "--verbose"]
    : ["--port", String(port), "--native-port", String(nativePort), "--native-driver", nativeDriver];
  const driverEnv = windows
    ? { ...env, TAURI_AUTOMATION: "true", TAURI_WEBVIEW_AUTOMATION: "true" }
    : env;
  captureDriverLog = true;
  driver = spawn(executable, args,
    { env: driverEnv, detached: !windows, stdio: ["ignore", "pipe", "pipe"] });
  const collectLog = (chunk) => {
    if (captureDriverLog) driverLog = (driverLog + chunk.toString()).slice(-1_048_576);
  };
  driver.stdout.on("data", collectLog);
  driver.stderr.on("data", collectLog);
  record("driver-start", { launch: launchNumber, executable, pid: driver.pid });
  driverError = undefined;
  driverExited = false;
  driverExit = new Promise((resolve) => {
    driver.on("error", (error) => {
      driverError = error;
      // A failed spawn has no process to reap. Other errors do not mean exit.
      if (!driver.pid) { driverExited = true; resolve(); }
    });
    driver.once("exit", (code, signal) => {
      record("driver-exit", { launch: launchNumber, code, signal });
      driverExited = true;
      resolve();
    });
  });
  await until(async () => {
    assert.equal(driver.exitCode, null, "tauri-driver exited");
    return (await request("GET", "/status")).ready;
  });
  const monitorAbort = new AbortController();
  const monitor = windows ? monitorWindowsLaunch(monitorAbort.signal) : Promise.resolve();
  let created;
  try {
    const capabilities = windows ? {
      browserName: "webview2", "ms:edgeChromium": true,
      "ms:edgeOptions": { binary: application, args: [] },
    } : { "tauri:options": { application } };
    record("session-launch", { launch: launchNumber, capabilities });
    created = await request("POST", "/session", { capabilities: { alwaysMatch: capabilities } });
  } finally {
    captureDriverLog = false;
    monitorAbort.abort();
    await monitor;
    await writeFile(path.join(evidenceDirectory, "driver-launch.log"), driverLog, { mode: 0o600 });
  }
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
    try {
      if (driver?.pid) {
        // CloseRequested deliberately hides this tray app. End the owned driver
        // process tree too; session deletion alone is not an application quit.
        if (process.platform === "win32") {
          if (!driverExited) {
            try {
              await promisify(execFile)(path.join(process.env.SystemRoot, "System32", "taskkill.exe"),
                ["/PID", String(driver.pid), "/T", "/F"], { timeout: 5000 });
            } catch (error) {
              // Native EdgeDriver has no tauri-driver Job wrapper. Surface tree
              // cleanup failures; killing only its parent is not sufficient.
              driver.kill("SIGKILL");
              throw error;
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
    } finally {
      if (process.platform === "win32") {
        // Also reap an app whose native driver exited before taskkill could
        // follow its tree. Restrict selection to this smoke's installed path.
        await promisify(execFile)("powershell.exe", ["-NoProfile", "-NonInteractive", "-EncodedCommand",
          Buffer.from(`
            $apps = @(Get-CimInstance Win32_Process -Filter "Name = 'private-ai-gateway-desktop.exe'" | Where-Object { $_.ExecutablePath -eq $env:TAURI_SMOKE_APPLICATION })
            foreach ($app in $apps) {
              & "$env:SystemRoot/System32/taskkill.exe" /PID $app.ProcessId /T /F
              if ($LASTEXITCODE -ne 0) { throw "Cannot clean up installed smoke app" }
            }
          `, "utf16le").toString("base64")], {
          env: { ...process.env, TAURI_SMOKE_APPLICATION: application }, timeout: 8000, maxBuffer: 65536,
        });
      }
      driver = undefined;
    }
  }
}

try {
  for (const directory of Object.values(xdgDirectories)) {
    await mkdir(directory, { recursive: true, mode: 0o700 });
  }
  await mkdir(data, { recursive: true, mode: 0o700 });
  await mkdir(evidenceDirectory, { recursive: true });
  record("smoke-start", { platform: process.platform, application, driverPath, nativeDriver });
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
  record("smoke-failure", { name: error.name, message: error.message });
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
  try { await shutdown(); } finally {
    try {
      if (process.platform === "win32") {
        try { await sampleWindowsProcesses(); } catch (error) {
          record("cleanup-snapshot-error", { message: error.message });
        }
      }
      await writeFile(path.join(evidenceDirectory, "driver-launch.log"), driverLog, { mode: 0o600 });
      await writeFile(path.join(evidenceDirectory, "launch-diagnostics.json"), JSON.stringify(diagnosticEvents, null, 2), { mode: 0o600 });
    } finally { await rm(home, { recursive: true, force: true }); }
  }
}
