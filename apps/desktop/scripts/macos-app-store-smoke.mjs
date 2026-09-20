#!/usr/bin/env node
import assert from "node:assert/strict";
import { execFile, execFileSync, spawn } from "node:child_process";
import { access, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import { MAC_APP_STORE_SIDECARS } from "./distribution.mjs";
import { appStoreRuntimeEntitlements } from "./package-app-store.mjs";

const appBundle = process.argv[2] && path.resolve(process.argv[2]);
assert.equal(process.platform, "darwin");
assert.ok(appBundle, "Supply the unsigned App Store app bundle path");

const exec = promisify(execFile);
const info = JSON.parse(
  execFileSync(
    "plutil",
    [
      "-convert",
      "json",
      "-o",
      "-",
      path.join(appBundle, "Contents/Info.plist"),
    ],
    { encoding: "utf8" },
  ),
);
const bundleName = path.basename(appBundle);
assert.match(bundleName, /^[A-Za-z0-9 ._-]+\.app$/);
const sourceExecutableDirectory = path.join(appBundle, "Contents/MacOS");
await access(path.join(sourceExecutableDirectory, info.CFBundleExecutable));
for (const name of MAC_APP_STORE_SIDECARS) {
  await access(path.join(sourceExecutableDirectory, name));
}

const scratch = await mkdtemp(path.join(os.tmpdir(), "pap-mas-smoke-"));
const smokeBundle = path.join(scratch, bundleName);
const executableDirectory = path.join(smokeBundle, "Contents/MacOS");
const appExecutable = path.join(executableDirectory, info.CFBundleExecutable);
const serviceExecutable = path.join(
  executableDirectory,
  "private-ai-proxy-service",
);

const delay = (milliseconds) =>
  new Promise((resolve) => setTimeout(resolve, milliseconds));

function processRows(stdout) {
  return stdout
    .split("\n")
    .map((line) => line.match(/^\s*(\d+)\s+(\d+)\s+(.+)$/))
    .filter(Boolean)
    .map((match) => ({
      pid: Number(match[1]),
      parentPid: Number(match[2]),
      command: match[3],
    }));
}

function runsExecutable(process, executable) {
  return (
    process.command === executable ||
    process.command.startsWith(`${executable} `)
  );
}

async function runningProcesses() {
  const { stdout } = await exec("ps", ["-axo", "pid=,ppid=,command="], {
    timeout: 2_000,
    maxBuffer: 4 * 1024 * 1024,
  });
  return processRows(stdout);
}

async function backendIsReady(pid) {
  try {
    const { stdout } = await exec(
      "/usr/sbin/lsof",
      ["-n", "-P", "-a", "-p", String(pid), "-U", "-Fn"],
      { timeout: 2_000, maxBuffer: 1024 * 1024 },
    );
    return stdout
      .split("\n")
      .some((line) => /\/backend\.sock(?:\s|$)/.test(line));
  } catch {
    return false;
  }
}

async function stop(pid, signal = "SIGTERM") {
  if (!pid) return;
  try {
    process.kill(pid, signal);
  } catch (error) {
    if (error?.code !== "ESRCH") throw error;
  }
}

async function diagnostic(log, launcherOutput) {
  const appOutput = await readFile(log, "utf8").catch(() => "");
  return `${launcherOutput}${appOutput}`.trim().slice(-16_384);
}

async function writePlist(file, value) {
  await writeFile(file, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 });
  await exec("plutil", ["-convert", "xml1", file], { timeout: 2_000 });
}

const log = path.join(scratch, "app.log");
let launcherOutput = "";
let appPid;
let servicePid;
let ownerExitPassed = false;
let launcher;

try {
  await exec("/usr/bin/ditto", [appBundle, smokeBundle], {
    timeout: 30_000,
    maxBuffer: 1024 * 1024,
  });
  // Distribution-signed MAS apps cannot launch before App Store processing.
  // Exercise the same binaries with only the unrestricted sandbox entitlements.
  const entitlements = appStoreRuntimeEntitlements();
  const mainEntitlements = path.join(scratch, "main.entitlements");
  const childEntitlements = path.join(scratch, "child.entitlements");
  await writePlist(mainEntitlements, entitlements.main);
  await writePlist(childEntitlements, entitlements.child);
  for (const name of MAC_APP_STORE_SIDECARS) {
    await exec(
      "/usr/bin/codesign",
      ["--force", "--sign", "-", "--entitlements", childEntitlements, path.join(executableDirectory, name)],
      { timeout: 30_000, maxBuffer: 1024 * 1024 },
    );
  }
  await exec(
    "/usr/bin/codesign",
    ["--force", "--sign", "-", "--entitlements", mainEntitlements, smokeBundle],
    { timeout: 30_000, maxBuffer: 1024 * 1024 },
  );
  await exec(
    "/usr/bin/codesign",
    ["--verify", "--deep", "--strict", "--verbose=2", smokeBundle],
    { timeout: 30_000, maxBuffer: 1024 * 1024 },
  );
  const existing = await runningProcesses();
  assert.equal(
    existing.some(
      (process) =>
        runsExecutable(process, appExecutable) ||
        runsExecutable(process, serviceExecutable),
    ),
    false,
    "App Store smoke requires no existing processes from its temporary app bundle",
  );
  launcher = spawn(
    "open",
    [
      "-F",
      "-W",
      "-g",
      "-n",
      "--stderr",
      log,
      smokeBundle,
    ],
    { stdio: ["ignore", "pipe", "pipe"] },
  );
  for (const stream of [launcher.stdout, launcher.stderr]) {
    stream.on("data", (bytes) => {
      launcherOutput = (launcherOutput + bytes.toString()).slice(-16_384);
    });
  }

  const readyDeadline = Date.now() + 30_000;
  while (Date.now() < readyDeadline && !servicePid) {
    assert.ok(
      launcher.exitCode === null && launcher.signalCode === null,
      `Launch Services exited before backend readiness. ${await diagnostic(log, launcherOutput)}`,
    );
    const processes = await runningProcesses();
    const app = processes.find((process) =>
      runsExecutable(process, appExecutable),
    );
    appPid = app?.pid;
    const service = appPid
      ? processes.find(
          (process) =>
            process.parentPid === appPid &&
            runsExecutable(process, serviceExecutable),
        )
      : undefined;
    if (service && (await backendIsReady(service.pid))) {
      servicePid = service.pid;
      break;
    }
    await delay(250);
  }
  assert.ok(
    servicePid,
    `App Store runtime backend readiness timed out. ${await diagnostic(log, launcherOutput)}`,
  );

  await stop(appPid);
  const exitDeadline = Date.now() + 10_000;
  while (Date.now() < exitDeadline) {
    const processes = await runningProcesses();
    const appRunning = processes.some((process) => process.pid === appPid);
    const serviceRunning = processes.some(
      (process) => process.pid === servicePid,
    );
    if (!appRunning && !serviceRunning) {
      console.log(
        "App Store runtime launch, backend readiness, and owner-exit cleanup passed",
      );
      ownerExitPassed = true;
      break;
    }
    await delay(250);
  }
  assert.ok(
    ownerExitPassed,
    "App Store runtime app or its sandboxed backend did not exit cleanly",
  );
} finally {
  const remaining = await runningProcesses().catch(() => []);
  for (const process of remaining) {
    if (
      runsExecutable(process, appExecutable) ||
      runsExecutable(process, serviceExecutable)
    ) {
      await stop(process.pid, "SIGKILL");
    }
  }
  if (launcher) {
    await new Promise((resolve) => {
      if (launcher.exitCode !== null || launcher.signalCode !== null) {
        resolve();
        return;
      }
      launcher.once("exit", resolve);
      launcher.kill("SIGTERM");
    });
  }
  await rm(scratch, { recursive: true, force: true });
}
