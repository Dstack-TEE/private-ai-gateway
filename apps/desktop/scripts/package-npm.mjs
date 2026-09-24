#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import {
  chmod,
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import semver from "semver";

import { binaries } from "./package-cli.mjs";

const scriptPath = fileURLToPath(import.meta.url);
const appRoot = path.resolve(path.dirname(scriptPath), "..");
const repositoryRoot = path.resolve(appRoot, "../..");
const wrapperTemplate = path.join(appRoot, "npm/private-ai-proxy");

export const npmPackageName = "private-ai-proxy";
export const npmPlatforms = {
  macos: "darwin",
  linux: "linux",
  windows: "win32",
};
export const npmArchitectures = ["arm64", "x64"];

// Like @openai/codex (codex-cli/scripts/build_npm_package.py), every platform
// payload is a version of the wrapper's own package name, such as
// private-ai-proxy@1.2.3-linux-x64, so the release needs one npm package and
// one trusted publisher. The wrapper depends on each payload through an npm
// alias named after the target.
export function platformPackageAlias(platform, arch) {
  return `${npmPackageName}-${platformTarget(platform, arch)}`;
}

export function platformPackageVersion(version, platform, arch) {
  validateNpmVersion(version);
  return validateNpmVersion(`${version}-${platformTarget(platform, arch)}`);
}

function platformTarget(platform, arch) {
  const npmPlatform = npmPlatforms[platform];
  if (!npmPlatform || !npmArchitectures.includes(arch)) {
    throw new Error(`Unsupported npm package target ${platform}-${arch}`);
  }
  return `${npmPlatform}-${arch}`;
}

export function validateNpmVersion(version) {
  const parsed = semver.parse(version);
  if (!parsed || semver.valid(version) !== version || parsed.build.length > 0) {
    throw new Error(`npm package version must be SemVer without build metadata, got ${JSON.stringify(version)}`);
  }
  return version;
}

export function wrapperManifest(version) {
  validateNpmVersion(version);
  const optionalDependencies = Object.keys(npmPlatforms).flatMap((platform) =>
    npmArchitectures.map((arch) => [
      platformPackageAlias(platform, arch),
      `npm:${npmPackageName}@${platformPackageVersion(version, platform, arch)}`,
    ]),
  );
  return {
    name: npmPackageName,
    version,
    description: "Local verifying gateway for confidential AI inference",
    license: "Apache-2.0",
    repository: {
      type: "git",
      url: "git+https://github.com/Dstack-TEE/private-ai-gateway.git",
      directory: "apps/desktop/npm/private-ai-proxy",
    },
    homepage: "https://github.com/Dstack-TEE/private-ai-gateway#readme",
    bugs: "https://github.com/Dstack-TEE/private-ai-gateway/issues",
    bin: {
      "private-ai-proxy": "bin/private-ai-proxy.cjs",
      pap: "bin/private-ai-proxy.cjs",
      aci: "bin/private-ai-proxy.cjs",
    },
    files: ["bin"],
    engines: { node: ">=18" },
    optionalDependencies: Object.fromEntries(optionalDependencies),
    publishConfig: { access: "public" },
  };
}

export function platformManifest({ platform, arch, version }) {
  const target = platformTarget(platform, arch);
  return {
    name: npmPackageName,
    version: platformPackageVersion(version, platform, arch),
    description: `Native Private AI Proxy binaries for ${target}`,
    license: "Apache-2.0",
    repository: {
      type: "git",
      url: "git+https://github.com/Dstack-TEE/private-ai-gateway.git",
    },
    homepage: "https://github.com/Dstack-TEE/private-ai-gateway#readme",
    os: [npmPlatforms[platform]],
    cpu: [arch],
    // The Linux binaries link against glibc 2.35+; there is no musl build.
    ...(platform === "linux" ? { libc: ["glibc"] } : {}),
    // Yarn PnP must extract the package so the executables exist on disk.
    preferUnplugged: true,
    files: ["vendor"],
    publishConfig: { access: "public" },
  };
}

export async function buildPlatformPackage({ platform, arch, version, source, output }) {
  const manifest = platformManifest({ platform, arch, version });
  await mkdir(output, { recursive: true });
  const scratch = await mkdtemp(path.join(output, ".pap-npm-platform-"));
  try {
    const vendor = path.join(scratch, "vendor");
    await mkdir(vendor);
    const extension = platform === "windows" ? ".exe" : "";
    for (const binary of binaries) {
      const sourceFile = path.join(source, `${binary}${extension}`);
      const metadata = await stat(sourceFile).catch(() => undefined);
      if (!metadata?.isFile()) throw new Error(`Missing npm source binary ${sourceFile}`);
      const destination = path.join(vendor, `${binary}${extension}`);
      await copyFile(sourceFile, destination);
      if (platform !== "windows") await chmod(destination, 0o755);
    }
    await writePackageFiles(
      scratch,
      manifest,
      `# private-ai-proxy ${manifest.version}\n\nThe native ${platformTarget(platform, arch)} binaries of [private-ai-proxy](https://www.npmjs.com/package/private-ai-proxy). Install \`private-ai-proxy\` instead of this version.\n`,
    );
    return packDirectory(scratch, output);
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
}

export async function buildWrapperPackage({ version, output }) {
  const manifest = wrapperManifest(version);
  await mkdir(output, { recursive: true });
  const scratch = await mkdtemp(path.join(output, ".pap-npm-wrapper-"));
  try {
    const bin = path.join(scratch, "bin");
    await mkdir(bin);
    const launcher = path.join(bin, "private-ai-proxy.cjs");
    await copyFile(path.join(wrapperTemplate, "bin/private-ai-proxy.cjs"), launcher);
    await chmod(launcher, 0o755);
    await writePackageFiles(
      scratch,
      manifest,
      await readFile(path.join(wrapperTemplate, "README.md"), "utf8"),
    );
    return packDirectory(scratch, output);
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
}

async function writePackageFiles(directory, manifest, readme) {
  await writeFile(path.join(directory, "package.json"), `${JSON.stringify(manifest, null, 2)}\n`);
  await writeFile(path.join(directory, "README.md"), readme);
  await copyFile(path.join(repositoryRoot, "LICENSE"), path.join(directory, "LICENSE"));
}

function packDirectory(directory, output) {
  const npm = process.platform === "win32" ? "npm.cmd" : "npm";
  const packed = JSON.parse(execFileSync(
    npm,
    ["pack", directory, "--json", "--pack-destination", output],
    { encoding: "utf8" },
  ));
  const entries = Array.isArray(packed) ? packed : Object.values(packed);
  if (entries.length !== 1 || typeof entries[0]?.filename !== "string") {
    throw new Error("npm pack did not report exactly one tarball");
  }
  return path.resolve(output, entries[0].filename);
}

function parseArguments(arguments_) {
  const [command, ...rest] = arguments_;
  if (!["platform", "wrapper"].includes(command) || rest.length % 2 !== 0) {
    throw new Error("Usage: package-npm.mjs <platform|wrapper> --version <semver> --output <dir> [--platform <macos|linux|windows> --arch <arm64|x64> --source <dir>]");
  }
  const values = new Map();
  for (let index = 0; index < rest.length; index += 2) {
    const key = rest[index];
    const value = rest[index + 1];
    if (!key.startsWith("--") || value === undefined || values.has(key.slice(2))) {
      throw new Error(`Invalid or duplicate argument ${JSON.stringify(key)}`);
    }
    values.set(key.slice(2), value);
  }
  const allowed = command === "platform"
    ? new Set(["platform", "arch", "version", "source", "output"])
    : new Set(["version", "output"]);
  const unknown = [...values.keys()].filter((key) => !allowed.has(key));
  if (unknown.length > 0) throw new Error(`Unknown argument --${unknown[0]}`);
  const version = validateNpmVersion(values.get("version"));
  const output = values.get("output");
  if (!output) throw new Error("--output is required");
  if (command === "wrapper") return { command, version, output: path.resolve(output) };
  const platform = values.get("platform");
  const arch = values.get("arch");
  platformTarget(platform, arch);
  const source = values.get("source");
  if (!source) throw new Error("--source is required for a platform package");
  return {
    command,
    platform,
    arch,
    version,
    source: path.resolve(source),
    output: path.resolve(output),
  };
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  const tarball = options.command === "wrapper"
    ? await buildWrapperPackage(options)
    : await buildPlatformPackage(options);
  console.log(`Packaged ${tarball}`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === scriptPath) {
  await main();
}
