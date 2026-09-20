#!/usr/bin/env node
import assert from "node:assert/strict";
import { execFile, execFileSync, spawn } from "node:child_process";
import { access, lstat, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";

const [appBundle, cli] = process.argv
  .slice(2)
  .map((value) => value && path.resolve(value));
assert.equal(process.platform, "darwin");
assert.ok(
  appBundle && cli,
  "Supply the signed app bundle and unsigned CLI paths",
);

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
const bundleIdentifier = info.CFBundleIdentifier;
const executable = path.join(
  appBundle,
  "Contents/MacOS",
  info.CFBundleExecutable,
);
assert.match(bundleIdentifier, /^[A-Za-z0-9.-]+$/);
await access(executable);
await access(cli);

const dataDirectories = [
  path.join(
    os.homedir(),
    "Library/Containers",
    bundleIdentifier,
    "Data/Library/Application Support",
    bundleIdentifier,
  ),
  path.join(os.homedir(), "Library/Application Support", bundleIdentifier),
];

async function exists(target) {
  try {
    await lstat(target);
    return true;
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
}

for (const directory of dataDirectories) {
  assert.equal(
    await exists(directory),
    false,
    `App Store smoke requires a clean runner: ${directory}`,
  );
}

const delay = (milliseconds) =>
  new Promise((resolve) => setTimeout(resolve, milliseconds));
let diagnostic = "";
const app = spawn(executable, [], { stdio: ["ignore", "pipe", "pipe"] });
for (const stream of [app.stdout, app.stderr]) {
  stream.on("data", (bytes) => {
    diagnostic = (diagnostic + bytes.toString()).slice(-16_384);
  });
}

async function status(dataDirectory) {
  try {
    const { stdout } = await exec(cli, ["status", "--json"], {
      env: { ...process.env, PRIVATE_AI_PROXY_DATA_DIR: dataDirectory },
      timeout: 1_000,
      maxBuffer: 1_048_576,
    });
    return JSON.parse(stdout);
  } catch {
    return undefined;
  }
}

async function stopApp() {
  if (app.exitCode !== null || app.signalCode !== null) return;
  const exited = new Promise((resolve) => app.once("exit", () => resolve(true)));
  app.kill("SIGTERM");
  if (!(await Promise.race([exited, delay(5_000).then(() => false)]))) {
    app.kill("SIGKILL");
    await exited;
  }
}

let activeDataDirectory;
try {
  const readyDeadline = Date.now() + 30_000;
  while (Date.now() < readyDeadline && !activeDataDirectory) {
    assert.equal(
      app.exitCode,
      null,
      `Signed App Store app exited before backend readiness. ${diagnostic}`,
    );
    for (const directory of dataDirectories) {
      if ((await status(directory))?.backend) {
        activeDataDirectory = directory;
        break;
      }
    }
    if (!activeDataDirectory) await delay(100);
  }
  assert.ok(
    activeDataDirectory,
    `Signed App Store backend readiness timed out. ${diagnostic}`,
  );
  await stopApp();
  const exitDeadline = Date.now() + 10_000;
  while (Date.now() < exitDeadline) {
    if (!(await status(activeDataDirectory))?.backend) {
      console.log(
        "Signed App Store launch, backend readiness, and owner-exit cleanup passed",
      );
      activeDataDirectory = undefined;
      break;
    }
    await delay(100);
  }
  assert.equal(
    activeDataDirectory,
    undefined,
    "Sandboxed backend outlived its owning App Store app",
  );
} finally {
  await stopApp();
  for (const directory of dataDirectories) {
    await rm(directory, { recursive: true, force: true });
  }
}
