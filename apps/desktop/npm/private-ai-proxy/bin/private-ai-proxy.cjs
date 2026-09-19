#!/usr/bin/env node
"use strict";

const { spawnSync } = require("node:child_process");
const { accessSync, constants } = require("node:fs");
const path = require("node:path");

const packages = {
  "darwin-arm64": "private-ai-proxy-darwin-arm64",
  "darwin-x64": "private-ai-proxy-darwin-x64",
  "linux-arm64": "private-ai-proxy-linux-arm64",
  "linux-x64": "private-ai-proxy-linux-x64",
  "win32-arm64": "private-ai-proxy-win32-arm64",
  "win32-x64": "private-ai-proxy-win32-x64",
};

function fail(message) {
  console.error(`private-ai-proxy: ${message}`);
  process.exitCode = 1;
}

function main() {
  const target = `${process.platform}-${process.arch}`;
  const packageName = packages[target];
  if (!packageName) {
    fail(`unsupported platform ${target}; supported platforms are ${Object.keys(packages).join(", ")}`);
    return;
  }

  let packageManifest;
  try {
    packageManifest = require.resolve(`${packageName}/package.json`);
  } catch (error) {
    if (error?.code !== "MODULE_NOT_FOUND") throw error;
    fail(
      `the optional package ${packageName} is missing. Reinstall private-ai-proxy without --omit=optional.`,
    );
    return;
  }

  const extension = process.platform === "win32" ? ".exe" : "";
  const executable = path.join(path.dirname(packageManifest), "vendor", `private-ai-proxy${extension}`);
  try {
    accessSync(executable, process.platform === "win32" ? constants.F_OK : constants.X_OK);
  } catch {
    fail(`the native executable is missing or not executable in ${packageName}`);
    return;
  }

  const result = spawnSync(executable, process.argv.slice(2), {
    stdio: "inherit",
    windowsHide: false,
  });
  if (result.error) throw result.error;
  if (result.signal) {
    process.kill(process.pid, result.signal);
    return;
  }
  process.exitCode = result.status ?? 1;
}

try {
  main();
} catch (error) {
  fail(error instanceof Error ? error.message : String(error));
}
