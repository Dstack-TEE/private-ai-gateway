#!/usr/bin/env node
import assert from "node:assert/strict";
import { execFile, execFileSync } from "node:child_process";
import { access, chmod, constants, mkdir, mkdtemp, rm, stat, writeFile } from "node:fs/promises";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";

import { binaries } from "./package-cli.mjs";

const [directory, expectedVersion] = process.argv.slice(2);
assert.ok(directory && path.isAbsolute(directory), "Supply an absolute CLI binary directory");
assert.match(expectedVersion ?? "", /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/, "Supply the package version");

const extension = process.platform === "win32" ? ".exe" : "";
const pag = path.join(directory, `pag${extension}`);
for (const name of binaries) {
  const file = path.join(directory, `${name}${extension}`);
  assert.ok((await stat(file)).isFile(), `Missing ${name}`);
  if (process.platform !== "win32") await access(file, constants.X_OK);
}

const version = execFileSync(pag, ["--version"], {
  encoding: "utf8",
  timeout: 10_000,
}).trim();
assert.equal(version, `pag ${expectedVersion}`);

const execute = promisify(execFile);
const home = await mkdtemp(path.join(os.tmpdir(), "pag-cli-smoke-"));
const data = path.join(home, ".private-ai-gateway");
const env = {
  ...process.env,
  PRIVATE_AI_GATEWAY_HOME: home,
  ACI_API_KEY: "",
  OPENAI_API_KEY: "",
  ANTHROPIC_API_KEY: "",
};
let ownedBackend;

const runJson = async (arguments_, timeout = 20_000) => {
  const { stdout } = await execute(pag, ["--json", ...arguments_], {
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
  const localApi = path.join(data, "local-api.json");
  await writeFile(localApi, `${JSON.stringify({
    listenAddress: "127.0.0.1",
    allowNetworkAccess: false,
    port,
  }, null, 2)}\n`, { mode: 0o600 });
  await chmod(localApi, 0o600);

  assert.equal((await runJson(["service", "status"])).status, "not_running");
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
  assert.equal(settings.preferences.appearance, "dark");
  assert.equal(settings.localApi.port, port);
  assert.equal(settings.localApi.listenAddress, "127.0.0.1");
  assert.equal(settings.localApi.allowNetworkAccess, false);

  assert.equal((await runJson(["--yes", "service", "stop"])).status, "stopped");
  assert.equal((await waitForNotRunning(10_000)).status, "not_running");
  assert.equal((await runJson(["service", "status"])).status, "not_running");
  ownedBackend = undefined;
  console.log(`CLI package: lifecycle passed with four sibling executables; ${version}`);
} finally {
  let cleanupError;
  try {
    if (ownedBackend) {
      const status = await runJson(["service", "status"]).catch(() => undefined);
      const stillOwned = status?.backend?.instanceId === ownedBackend.instanceId
        || (status === undefined && processExists(ownedBackend.processId));
      if (stillOwned) {
        await runJson(["--yes", "service", "stop"]).catch(() => undefined);
        if (!(await waitForExit(ownedBackend.processId, 5_000))) {
          process.kill(ownedBackend.processId, "SIGTERM");
          if (!(await waitForExit(ownedBackend.processId, 5_000))) {
            process.kill(ownedBackend.processId, "SIGKILL");
            assert.ok(await waitForExit(ownedBackend.processId, 5_000), "Owned backend did not exit");
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
