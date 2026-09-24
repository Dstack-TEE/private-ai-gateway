#!/usr/bin/env node
"use strict";

const { spawnSync } = require("node:child_process");
const { accessSync, constants, realpathSync } = require("node:fs");
const path = require("node:path");

// Each target's native binaries are a version of this package, such as
// private-ai-proxy@1.2.3-linux-x64, installed through an optional dependency
// alias such as private-ai-proxy-linux-x64. The package manager installs only
// the one whose os/cpu/libc match, as for @openai/codex's bin/codex.js.
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
    alias: `private-ai-proxy-${target}`,
    target,
    executable: `vendor/private-ai-proxy${platform === "win32" ? ".exe" : ""}`,
  };
}

// The global reinstall command for the package manager that owns this copy,
// detected from its install path as bin/codex.js does.
function reinstallCommand(version) {
  const spec = `private-ai-proxy@${version}`;
  let location = __dirname;
  try {
    location = realpathSync(__dirname);
  } catch {
    // Fall back to the lexical path.
  }
  if (/[\\/]\.bun[\\/]install[\\/]global[\\/]/.test(location)) return `bun add --global ${spec}`;
  if (/[\\/]\.pnpm[\\/]/.test(location)) return `pnpm add --global ${spec}`;
  return `npm install --global ${spec} --include=optional`;
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

  const version = require("../package.json").version;
  const expectedVersion = `${version}-${platform.target}`;
  let manifestPath;
  try {
    manifestPath = require.resolve(`${platform.alias}/package.json`);
  } catch (error) {
    if (error?.code !== "MODULE_NOT_FOUND") throw error;
    fail(isLinuxWithoutGlibc()
      ? muslMessage
      : `the native binaries for ${target} (private-ai-proxy@${expectedVersion}) are not installed. `
        + "Optional dependencies were omitted (--omit=optional or --no-optional) or their download failed. "
        + `Reinstall with: ${reinstallCommand(version)}`);
    return;
  }

  // An update that could not replace the native package, for example because
  // a running process locked its files on Windows, leaves an older version.
  const installedVersion = require(manifestPath).version;
  if (installedVersion !== expectedVersion) {
    fail(`the installed native binaries are private-ai-proxy@${installedVersion}, expected ${expectedVersion}. `
      + `Stop any running private-ai-proxy processes, then reinstall with: ${reinstallCommand(version)}`);
    return;
  }

  const executable = path.join(path.dirname(manifestPath), platform.executable);
  try {
    accessSync(executable, process.platform === "win32" ? constants.F_OK : constants.X_OK);
  } catch {
    fail(`the native executable is missing or not executable in ${path.dirname(manifestPath)}. `
      + `Reinstall with: ${reinstallCommand(version)}`);
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
