#!/usr/bin/env node
// Linux native-window/process smoke; renderer assertions run separately.
import assert from "node:assert/strict";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { createServer } from "node:net";

const [binaries] = process.argv.slice(2);
assert.equal(process.platform, "linux");
assert.ok(binaries && path.isAbsolute(binaries), "Supply an absolute installed binary directory");
const exec = promisify(execFile);
const home = await mkdtemp(path.join(tmpdir(), "pap-native-"));
const env = { ...process.env, HOME: home, XDG_CONFIG_HOME: path.join(home, "config"), XDG_DATA_HOME: path.join(home, "data"), XDG_CACHE_HOME: path.join(home, "cache"), PRIVATE_AI_PROXY_HOME: home };
const children = [];
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const cli = async (...args) => JSON.parse((await exec(path.join(binaries, "pap"), [...args, "--json"], { env, timeout: 25_000, maxBuffer: 1_048_576 })).stdout);

function start(name) {
  const child = spawn(path.join(binaries, name), [], { env, detached: true, stdio: ["ignore", "pipe", "pipe"] });
  child.diagnostic = "";
  child.stdout.resume();
  child.stderr.on("data", (bytes) => { child.diagnostic = (child.diagnostic + bytes.toString()).slice(-8_192); });
  children.push(child);
  return child;
}

async function stop(child) {
  const signal = (name) => {
    try { process.kill(-child.pid, name); } catch (error) { if (error.code !== "ESRCH") throw error; }
  };
  signal("SIGTERM");
  for (let count = 0; count < 100; count++) {
    if (child.exitCode !== null || child.signalCode !== null) break;
    await delay(50);
  }
  signal("SIGKILL");
  if (child.exitCode === null && child.signalCode === null) {
    await new Promise((resolve) => child.once("exit", resolve));
  }
}

try {
  const server = createServer();
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  assert.ok(address && typeof address !== "string");
  await new Promise((resolve) => server.close(resolve));
  const data = path.join(home, ".private-ai-proxy");
  await mkdir(data, { mode: 0o700 });
  await writeFile(path.join(data, "local-api.json"), JSON.stringify({ listenAddress: "127.0.0.1", allowNetworkAccess: false, port: address.port }), { mode: 0o600 });
  const backend = start("pap-service");
  let state;
  for (let count = 0; count < 100; count++) {
    assert.equal(backend.exitCode, null, "Backend exited before readiness");
    state = await cli("status");
    if (state.backend) break;
    await delay(100);
  }
  assert.ok(state.backend, "Backend readiness timed out");
  const instance = state.backend.instanceId;
  let ui = start("private-ai-proxy-desktop");
  let windows = "";
  for (let count = 0; count < 150; count++) {
    assert.equal(ui.exitCode, null, `UI exited before creating a window: ${ui.diagnostic}`);
    windows = (await exec("xwininfo", ["-root", "-tree"], { env, timeout: 5_000 })).stdout;
    if (windows.includes("Private AI Proxy")) break;
    await delay(100);
  }
  assert.match(windows, /Private AI Proxy/, ui.diagnostic);
  await stop(ui);
  assert.equal((await cli("status")).backend.instanceId, instance);
  ui = start("private-ai-proxy-desktop");
  await delay(1_000);
  assert.equal(ui.exitCode, null);
  assert.equal((await cli("status")).backend.instanceId, instance);
  await cli("settings", "set", "appearance", "dark", "--yes");
  assert.equal((await cli("settings", "show")).preferences.appearance, "dark");
  await stop(ui);
  await cli("service", "stop", "--yes");
  assert.equal((await cli("status")).status, "not_running");
  console.log("Native window/process smoke passed: UI termination, same-backend reattachment, CLI mutation, explicit service shutdown");
} finally {
  for (const child of children.reverse()) await stop(child);
  await rm(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
}
