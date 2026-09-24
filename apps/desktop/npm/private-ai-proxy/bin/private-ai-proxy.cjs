#!/usr/bin/env node
"use strict";

const { spawnSync } = require("node:child_process");
const { accessSync, constants } = require("node:fs");
const path = require("node:path");

// The native binaries ship in one optional dependency per target, and the
// package manager installs only the one whose os/cpu/libc match, as esbuild's
// lib/npm/node-platform.ts and Biome's bin/biome resolve theirs.
const supportedTargets = [
  "darwin-arm64",
  "darwin-x64",
  "linux-arm64",
  "linux-x64",
  "win32-arm64",
  "win32-x64",
];
const muslMessage =
  "Linux builds require glibc 2.35 or newer; musl-based distributions such as Alpine are not supported";

function platformPackage(platform, arch) {
  const target = `${platform}-${arch}`;
  if (!supportedTargets.includes(target)) return undefined;
  return {
    name: `@phala/private-ai-proxy-${target}`,
    executable: `vendor/private-ai-proxy${platform === "win32" ? ".exe" : ""}`,
  };
}

// Only consulted after a failure: npm skips the glibc package on musl, and
// package managers that ignore `libc` install a binary musl cannot load.
function isLinuxWithoutGlibc() {
  if (process.platform !== "linux") return false;
  try {
    process.report.excludeNetwork = true;
    return !process.report.getReport().header.glibcVersionRuntime;
  } catch {
    return false;
  }
}

function fail(message) {
  console.error(`private-ai-proxy: ${message}`);
  process.exitCode = 1;
}

function main() {
  const target = `${process.platform}-${process.arch}`;
  const platform = platformPackage(process.platform, process.arch);
  if (!platform) {
    fail(`unsupported platform ${target}; supported platforms are ${supportedTargets.join(", ")}`);
    return;
  }

  let manifestPath;
  try {
    manifestPath = require.resolve(`${platform.name}/package.json`);
  } catch (error) {
    if (error?.code !== "MODULE_NOT_FOUND") throw error;
    fail(isLinuxWithoutGlibc()
      ? muslMessage
      : `the optional dependency ${platform.name} is not installed. Reinstall private-ai-proxy with optional dependencies enabled (without --omit=optional or --no-optional).`);
    return;
  }

  const expectedVersion = require("../package.json").version;
  const installedVersion = require(manifestPath).version;
  if (installedVersion !== expectedVersion) {
    fail(`${platform.name}@${installedVersion} does not match private-ai-proxy@${expectedVersion}; reinstall private-ai-proxy`);
    return;
  }

  const executable = path.join(path.dirname(manifestPath), platform.executable);
  try {
    accessSync(executable, process.platform === "win32" ? constants.F_OK : constants.X_OK);
  } catch {
    fail(`the native executable is missing or not executable in ${platform.name}`);
    return;
  }

  const result = spawnSync(executable, process.argv.slice(2), {
    stdio: "inherit",
    windowsHide: false,
  });
  if (result.error) {
    if (result.error.code === "ENOENT" && isLinuxWithoutGlibc()) {
      fail(muslMessage);
      return;
    }
    throw result.error;
  }
  if (result.signal) {
    process.kill(process.pid, result.signal);
    return;
  }
  process.exitCode = result.status ?? 1;
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    fail(error instanceof Error ? error.message : String(error));
  }
}

module.exports = { platformPackage, supportedTargets };
