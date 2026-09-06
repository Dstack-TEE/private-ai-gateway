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
let app;
let appExit;
let appError;
let appExited;
let session;
let base;
let launchNumber = 0;
let captureDriverLog = false;
let driverLog = "";
const diagnosticEvents = [];
let webviewDataDirectory;

function record(event, details = {}) {
  diagnosticEvents.push({ at: new Date().toISOString(), event, ...details });
}

async function windowsCommand(script, variables) {
  return promisify(execFile)("powershell.exe", ["-NoProfile", "-NonInteractive", "-EncodedCommand",
    Buffer.from(`$ErrorActionPreference = 'Stop'; ${script}`, "utf16le").toString("base64")], {
    env: { ...process.env, ...variables }, timeout: 8000, maxBuffer: 65536,
  });
}

async function waitForCdp(port) {
  const deadline = Date.now() + 30_000;
  let targets;
  await until(async () => {
    const response = await fetch(`http://127.0.0.1:${port}/json/list`, { signal: AbortSignal.timeout(1000) });
    // Even a non-CDP HTTP listener must reach the ownership check, rather
    // than turning a port conflict into a generic readiness timeout.
    await response.body?.cancel();
    return true;
  });
  // A released ephemeral port can be taken by another process. Verify the
  // listener belongs to this app's tree before allowing WebDriver to attach.
  await windowsCommand(`
    $listeners = @(Get-NetTCPConnection -State Listen -LocalPort $env:TAURI_SMOKE_CDP_PORT)
    if (!$listeners.Count) { throw 'CDP listener disappeared' }
    foreach ($listener in $listeners) {
      if ($listener.LocalAddress -notin @('127.0.0.1', '::1')) { throw 'CDP is not loopback-only' }
      $owner = $listener.OwningProcess
      for ($depth = 0; $depth -lt 8 -and $owner -ne [int]$env:TAURI_SMOKE_APP_PID; $depth++) {
        $owner = (Get-CimInstance Win32_Process -Filter "ProcessId = $owner").ParentProcessId
      }
      if ($owner -ne [int]$env:TAURI_SMOKE_APP_PID) { throw 'CDP port conflict: listener is outside the owned app tree' }
    }
  `, { TAURI_SMOKE_CDP_PORT: String(port), TAURI_SMOKE_APP_PID: String(app.pid) });
  await until(async () => {
    const response = await fetch(`http://127.0.0.1:${port}/json/list`, { signal: AbortSignal.timeout(1000) });
    targets = await response.json();
    return response.ok && Array.isArray(targets) && targets.some((target) => target.type === "page");
  }, Math.max(0, deadline - Date.now()));
  record("cdp-ready", { launch: launchNumber, targetCount: targets.length });
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

async function until(check, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    if (appError) throw appError;
    if (appExited) throw new Error(`Installed app exited with status ${app.exitCode}`);
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
  driverError = undefined;
  driverExited = false;
  const port = await freePort();
  const nativePort = await freePort();
  assert.notEqual(port, nativePort, "WebDriver ports must be distinct");
  base = `http://127.0.0.1:${port}`;
  // Use Microsoft's attach contract on the same installed release executable.
  // https://learn.microsoft.com/microsoft-edge/webview2/how-to/webdriver
  // https://learn.microsoft.com/microsoft-edge/webdriver/capabilities-edge-options
  const windows = process.platform === "win32";
  let cdpPort;
  if (windows) {
    webviewDataDirectory = path.join(home, `webview-${launchNumber}`);
    await mkdir(webviewDataDirectory, { mode: 0o700 });
    cdpPort = await freePort();
    assert.ok(cdpPort !== port && cdpPort !== nativePort, "CDP and driver ports must be distinct");
    appError = undefined;
    appExited = false;
    app = spawn(application, [], { env: {
      ...env,
      WEBVIEW2_USER_DATA_FOLDER: webviewDataDirectory,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-address=127.0.0.1 --remote-debugging-port=${cdpPort}`,
    }, stdio: "ignore" });
    record("app-start", { launch: launchNumber, pid: app.pid });
    appExit = new Promise((resolve) => {
      app.on("error", (error) => {
        appError = error;
        if (!app.pid) { appExited = true; resolve(); }
      });
      app.once("exit", (code, signal) => {
        record("app-exit", { launch: launchNumber, code, signal });
        appExited = true;
        resolve();
      });
    });
    await waitForCdp(cdpPort);
  }
  const executable = windows ? nativeDriver : driverPath;
  const args = windows ? [`--port=${port}`, "--host=127.0.0.1"]
    : ["--port", String(port), "--native-port", String(nativePort), "--native-driver", nativeDriver];
  // Windows verbose driver logs contain CDP websocket URLs and IPC payloads.
  captureDriverLog = !windows;
  driver = spawn(executable, args,
    { env, detached: !windows, stdio: ["ignore", "pipe", "pipe"] });
  const collectLog = (chunk) => {
    if (captureDriverLog) driverLog = (driverLog + chunk.toString()).slice(-1_048_576);
  };
  driver.stdout.on("data", collectLog);
  driver.stderr.on("data", collectLog);
  record("driver-start", { launch: launchNumber, executable, pid: driver.pid });
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
  let created;
  try {
    const capabilities = windows ? {
      browserName: "webview2", "ms:edgeChromium": true,
      "ms:edgeOptions": { debuggerAddress: `127.0.0.1:${cdpPort}` },
    } : { "tauri:options": { application } };
    record("session-launch", { launch: launchNumber, mode: windows ? "attach" : "launch" });
    created = await request("POST", "/session", { capabilities: { alwaysMatch: capabilities } });
  } finally {
    captureDriverLog = false;
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

async function reap(exit, label) {
  let timer;
  try {
    await Promise.race([exit, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`Timed out reaping the owned ${label}`)), 5000);
    })]);
  } finally { clearTimeout(timer); }
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
              await reap(driverExit, "driver");
              throw error;
            }
          }
        } else {
          try { process.kill(-driver.pid, "SIGKILL"); } catch (error) { if (error.code !== "ESRCH") throw error; }
        }
        await reap(driverExit, "driver");
      }
    } finally {
      if (app?.pid) {
        // Attach session deletion does not own the app. Kill it independently,
        // including orphaned WebView2 processes identified by our private UDF.
        await windowsCommand(`
          $all = @(Get-CimInstance Win32_Process -Filter "Name = 'private-ai-gateway-desktop.exe' OR Name = 'msedgewebview2.exe'")
          $isOwned = {
            ($_.ProcessId -eq [int]$env:TAURI_SMOKE_APP_PID -and $_.ExecutablePath -eq $env:TAURI_SMOKE_APPLICATION) -or
            ($_.Name -eq 'msedgewebview2.exe' -and $_.CommandLine -and $_.CommandLine.Contains($env:TAURI_SMOKE_UDF))
          }
          $owned = @($all | Where-Object $isOwned)
          foreach ($process in $owned) {
            if (Get-Process -Id $process.ProcessId -ErrorAction SilentlyContinue) {
              & "$env:SystemRoot/System32/taskkill.exe" /PID $process.ProcessId /T /F | Out-Null
              if ($LASTEXITCODE -ne 0 -and (Get-Process -Id $process.ProcessId -ErrorAction SilentlyContinue)) {
                throw 'Cannot clean up the owned app or WebView2 process'
              }
            }
          }
          $remaining = @(Get-CimInstance Win32_Process -Filter "Name = 'private-ai-gateway-desktop.exe' OR Name = 'msedgewebview2.exe'" | Where-Object $isOwned)
          if ($remaining.Count) { throw 'Owned app tree remains; retaining private UDF' }
        `, { TAURI_SMOKE_APP_PID: String(app.pid), TAURI_SMOKE_APPLICATION: path.normalize(application),
          TAURI_SMOKE_UDF: path.normalize(webviewDataDirectory) });
        await reap(appExit, "app");
        record("app-tree-cleaned", { launch: launchNumber });
      }
      app = undefined;
      appExited = false;
      appError = undefined;
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
  record("smoke-failure", { name: error.name });
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
  let cleaned = false;
  try { await shutdown(); cleaned = true; } finally {
    try {
      await writeFile(path.join(evidenceDirectory, "driver-launch.log"), driverLog, { mode: 0o600 });
      await writeFile(path.join(evidenceDirectory, "launch-diagnostics.json"), JSON.stringify(diagnosticEvents, null, 2), { mode: 0o600 });
    } finally { if (cleaned) await rm(home, { recursive: true, force: true }); }
  }
}
