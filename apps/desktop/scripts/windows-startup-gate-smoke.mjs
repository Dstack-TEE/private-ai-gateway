#!/usr/bin/env node
import assert from "node:assert/strict";
import { execFile, execFileSync, spawn } from "node:child_process";
import { once } from "node:events";
import { access, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import { stagePortable } from "./package-cli.mjs";

assert.equal(process.platform, "win32");
const [makeNsis] = process.argv.slice(2);
assert.ok(makeNsis && path.isAbsolute(makeNsis), "Supply the absolute makensis.exe path");
await access(makeNsis);

const execute = promisify(execFile);
const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const targetTriple = execFileSync("rustc", ["-vV"], { encoding: "utf8", timeout: 10_000 }).match(/^host: (.+)$/m)?.[1];
assert.ok(targetTriple?.includes("windows"), "Expected a Windows Rust host target");
const { identifier } = JSON.parse(await readFile(path.join(appRoot, "src-tauri/tauri.brand.conf.json"), "utf8"));
assert.match(identifier, /^[A-Za-z0-9.-]+$/);

const scratch = await mkdtemp(path.join(os.tmpdir(), "pap-windows-gate-"));
const portable = path.join(scratch, "bin");
const appData = path.join(scratch, "appdata");
const profile = path.join(scratch, "profile");
const data = path.join(appData, identifier);
const acquired = path.join(scratch, "acquired");
const release = path.join(scratch, "release");
const fixture = path.join(scratch, "gate-fixture.exe");
const pap = path.join(portable, "private-ai-proxy.exe");
const env = {
  ...process.env,
  APPDATA: appData,
  USERPROFILE: profile,
  HOME: profile,
  PRIVATE_AI_PROXY_HOME: "",
  PAP_GATE_ACQUIRED: acquired,
  PAP_GATE_RELEASE: release,
  ACI_API_KEY: "",
  OPENAI_API_KEY: "",
  ANTHROPIC_API_KEY: "",
};
let gate;
let gateSpawned = false;
let backendAttempted = false;

const waitFor = async (check, timeout, message) => {
  const deadline = Date.now() + timeout;
  while (!(await check()) && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 50));
  assert.ok(await check(), message);
};

const waitForExit = async (child, timeout) => {
  if (child.exitCode !== null || child.signalCode !== null) return;
  await once(child, "exit", { signal: AbortSignal.timeout(timeout) });
};

const expectGateFailure = async (arguments_, timeout) => {
  let failure;
  try {
    await execute(pap, ["--json", ...arguments_], { env, timeout, windowsHide: true });
  } catch (error) {
    failure = error;
  }
  assert.ok(failure && !failure.killed, `${arguments_.join(" ")} did not fail cleanly`);
  assert.match(`${failure.stdout ?? ""}\n${failure.stderr ?? ""}`, /Backend startup or an update is already in progress/);
};

const reservePort = async () => {
  const server = net.createServer();
  await new Promise((resolve, reject) => server.once("error", reject).listen(0, "127.0.0.1", resolve));
  const port = server.address().port;
  await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  return port;
};

try {
  await stagePortable({
    sourceDir: path.join(appRoot, "src-tauri/binaries"),
    targetTriple,
    platform: "windows",
    destination: portable,
  });
  const nsisEnv = Object.fromEntries(Object.entries(env).filter(
    ([key]) => !["NSISDIR", "NSISCONFDIR"].includes(key.toUpperCase()),
  ));
  await execute(makeNsis, [
    `-DBUNDLEID=${identifier}`,
    `-DHOOKS_DIR=${path.join(appRoot, "src-tauri/installer")}`,
    `-DOUTPUT=${fixture}`,
    path.join(appRoot, "src-tauri/installer/windows-startup-gate-fixture.nsi"),
  ], { env: nsisEnv, timeout: 20_000, windowsHide: true });

  gate = spawn(fixture, ["/S"], { env, stdio: "ignore", windowsHide: true });
  await once(gate, "spawn");
  gateSpawned = true;
  await waitFor(() => access(acquired).then(() => true, () => false), 10_000, "NSIS did not acquire startup.lock");
  await expectGateFailure(["app", "open"], 5_000);
  backendAttempted = true;
  await expectGateFailure(["service", "start"], 20_000);

  await writeFile(release, "release");
  await waitForExit(gate, 10_000);
  assert.equal(gate.exitCode, 0);

  await mkdir(data, { recursive: true });
  await writeFile(path.join(data, "local-api.json"), JSON.stringify({
    listenAddress: "127.0.0.1",
    allowNetworkAccess: false,
    port: await reservePort(),
  }));
  const started = JSON.parse((await execute(pap, ["--json", "service", "start"], { env, timeout: 20_000, windowsHide: true })).stdout);
  assert.ok(Number.isInteger(started.processId) && started.processId > 0);
} finally {
  let cleanupError;
  await writeFile(release, "release").catch((error) => { cleanupError = error; });
  if (gateSpawned && gate.exitCode === null && gate.signalCode === null) {
    gate.kill();
    await waitForExit(gate, 2_000).catch((error) => { cleanupError ??= error; });
  }
  if (backendAttempted) {
    await execute(pap, ["--json", "--yes", "service", "stop"], {
      env, timeout: 10_000, windowsHide: true,
    }).catch((error) => {
      const output = `${error.stdout ?? ""}\n${error.stderr ?? ""}`.trim();
      cleanupError ??= new Error(`Could not stop the test-owned backend${output ? `: ${output}` : ""}`);
    });
  }
  if (cleanupError) {
    console.error(`Preserving failed startup-gate state at ${scratch}`);
    throw cleanupError;
  }
  await rm(scratch, { recursive: true, force: true });
}
