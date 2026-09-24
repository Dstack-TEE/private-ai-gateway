#!/usr/bin/env node
import assert from "node:assert/strict";
import { execFile, execFileSync } from "node:child_process";
import { access, chmod, constants, mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";

import { aliases, binaries } from "./package-cli.mjs";

const [directory, expectedVersion, aliasDirectory = directory] = process.argv.slice(2);
assert.ok(directory && path.isAbsolute(directory), "Supply an absolute CLI binary directory");
assert.match(expectedVersion ?? "", /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/, "Supply the package version");

const extension = process.platform === "win32" ? ".exe" : "";
const pap = path.join(directory, `private-ai-proxy${extension}`);
for (const name of binaries) {
  const file = path.join(directory, `${name}${extension}`);
  assert.ok((await stat(file)).isFile(), `Missing ${name}`);
  if (process.platform !== "win32") await access(file, constants.X_OK);
}

const version = execFileSync(pap, ["--version"], {
  encoding: "utf8",
  timeout: 10_000,
}).trim();
assert.equal(version, `private-ai-proxy ${expectedVersion}`);
for (const alias of aliases) {
  const aliasVersion = process.platform === "win32"
    ? execFileSync(process.env.ComSpec ?? "cmd.exe", ["/d", "/s", "/c", `""${path.join(aliasDirectory, `${alias}.cmd`)}" --version"`], { encoding: "utf8", timeout: 10_000, windowsVerbatimArguments: true })
    : execFileSync(path.join(aliasDirectory, alias), ["--version"], { encoding: "utf8", timeout: 10_000 });
  assert.equal(aliasVersion.trim(), version, `${alias} must resolve to the canonical CLI`);
}

const execute = promisify(execFile);
const home = await mkdtemp(path.join(os.tmpdir(), "pap-cli-smoke-"));
const data = path.join(home, ".private-ai-proxy");
const env = {
  ...process.env,
  PRIVATE_AI_PROXY_HOME: home,
  ACI_API_KEY: "",
  OPENAI_API_KEY: "",
  ANTHROPIC_API_KEY: "",
};
let ownedBackend;
let startAttempted = false;

const runJson = async (arguments_, timeout = 20_000) => {
  console.log(`CLI lifecycle: ${arguments_.join(" ")}`);
  const { stdout } = await execute(pap, ["--json", ...arguments_], {
    cwd: directory,
    env,
    encoding: "utf8",
    timeout,
    windowsHide: true,
  });
  return JSON.parse(stdout);
};

const reserveLoopbackPort = async () => {
  const server = net.createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  assert.ok(address && typeof address !== "string");
  await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  return address.port;
};

const processExists = (pid) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    if (error?.code === "ESRCH") return false;
    throw error;
  }
};

const waitForExit = async (pid, timeout) => {
  const deadline = Date.now() + timeout;
  while (processExists(pid) && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  return !processExists(pid);
};

const waitForNotRunning = async (timeout) => {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const status = await runJson(["service", "status"]);
    if (status.status === "not_running") return status;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  assert.fail("Backend did not report not_running after stop");
};

try {
  const port = await reserveLoopbackPort();
  await mkdir(data, { recursive: true, mode: 0o700 });
  await chmod(data, 0o700);
  await mkdir(path.join(data, "Config"), { mode: 0o700 });
  const config = path.join(data, "Config", "config.toml");
  await writeFile(config, `[localApi]\nport = ${port}\n`, { mode: 0o600 });
  await chmod(config, 0o600);

  assert.equal((await runJson(["service", "status"])).status, "not_running");
  startAttempted = true;
  ownedBackend = await runJson(["service", "start"]);
  assert.equal(ownedBackend.version, expectedVersion);
  assert.ok(Number.isInteger(ownedBackend.processId) && ownedBackend.processId > 0);
  assert.ok(typeof ownedBackend.instanceId === "string" && ownedBackend.instanceId.length > 0);

  const running = await runJson(["service", "status"]);
  assert.equal(running.backend.instanceId, ownedBackend.instanceId);
  assert.equal(running.backend.processId, ownedBackend.processId);

  const changed = await runJson(["--yes", "settings", "set", "appearance", "dark"]);
  assert.equal(changed.appearance, "dark");
  const settings = await runJson(["settings", "show"]);
  assert.equal(settings.settings.appearance, "dark");
  assert.equal(settings.settings.localApi.port, port);
  assert.equal(settings.settings.localApi.listenAddress, "127.0.0.1");
  assert.equal(settings.settings.localApi.allowNetworkAccess, false);
  assert.equal(settings.files.config, config);
  // Written in place: the hand-written table is kept as it was.
  const saved = await readFile(config, "utf8");
  assert.ok(saved.includes('appearance = "dark"\n') && saved.includes(`[localApi]\nport = ${port}\n`), saved);

  assert.equal((await runJson(["--yes", "service", "stop"])).status, "stopped");
  assert.equal((await waitForNotRunning(10_000)).status, "not_running");
  assert.equal((await runJson(["service", "status"])).status, "not_running");
  ownedBackend = undefined;
  startAttempted = false;
  console.log(`CLI package: lifecycle passed with three sibling executables; ${version}`);
} finally {
  let cleanupError;
  try {
    if (startAttempted) {
      // Every backend in this isolated home is test-owned, including one whose
      // start timed out before reporting it.
      const status = await runJson(["service", "status"]).catch(() => undefined);
      const processId = status?.backend?.processId
        ?? (status === undefined ? ownedBackend?.processId : undefined);
      if (processId && processExists(processId)) {
        await runJson(["--yes", "service", "stop"]).catch(() => undefined);
        if (!(await waitForExit(processId, 5_000))) {
          process.kill(processId, "SIGTERM");
          if (!(await waitForExit(processId, 5_000))) {
            process.kill(processId, "SIGKILL");
            assert.ok(await waitForExit(processId, 5_000), "Owned backend did not exit");
          }
        }
      }
    }
  } catch (error) {
    cleanupError = error;
  }
  await rm(home, { recursive: true, force: true });
  if (cleanupError) throw cleanupError;
}
