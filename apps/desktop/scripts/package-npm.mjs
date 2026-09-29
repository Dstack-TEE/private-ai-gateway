#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { chmod, copyFile, mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
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
      aci: "bin/aci.cjs",
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
  const readme = `# private-ai-proxy ${manifest.version}\n\nThe native ${platformTarget(platform, arch)} binaries of [private-ai-proxy](https://www.npmjs.com/package/private-ai-proxy). Install \`private-ai-proxy\` instead of this version.\n`;
  return pack(output, manifest, readme, async (directory) => {
    const vendor = path.join(directory, "vendor");
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
  });
}

export async function buildWrapperPackage({ version, output }) {
  const readme = await readFile(path.join(wrapperTemplate, "README.md"), "utf8");
  return pack(output, wrapperManifest(version), readme, async (directory) => {
    const bin = path.join(directory, "bin");
    await mkdir(bin);
    for (const name of ["private-ai-proxy.cjs", "aci.cjs"]) {
      await copyFile(path.join(wrapperTemplate, "bin", name), path.join(bin, name));
      await chmod(path.join(bin, name), 0o755);
    }
  });
}

// Packs a package directory that `addFiles` fills besides its manifest,
// README and LICENSE into a tarball under `output`.
async function pack(output, manifest, readme, addFiles) {
  await mkdir(output, { recursive: true });
  const directory = await mkdtemp(path.join(output, ".pap-npm-"));
  try {
    await addFiles(directory);
    await writeFile(path.join(directory, "package.json"), `${JSON.stringify(manifest, null, 2)}\n`);
    await writeFile(path.join(directory, "README.md"), readme);
    await copyFile(path.join(repositoryRoot, "LICENSE"), path.join(directory, "LICENSE"));
    return packDirectory(directory, output);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
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

function parseArguments([command, ...args]) {
  if (!["platform", "wrapper"].includes(command)) {
    throw new Error("Usage: package-npm.mjs <platform|wrapper> --version <semver> --output <dir> [--platform <macos|linux|windows> --arch <arm64|x64> --source <dir>]");
  }
  const option = { type: "string" };
  const options = { version: option, output: option, ...(command === "platform" ? { platform: option, arch: option, source: option } : {}) };
  const { values, tokens } = parseArgs({ args, options, tokens: true });
  const names = tokens.map((token) => token.name);
  if (new Set(names).size !== names.length) throw new Error("Duplicate argument");
  const version = validateNpmVersion(values.version);
  if (!values.output) throw new Error("--output is required");
  const output = path.resolve(values.output);
  if (command === "wrapper") return { command, version, output };
  platformTarget(values.platform, values.arch);
  if (!values.source) throw new Error("--source is required for a platform package");
  return { command, platform: values.platform, arch: values.arch, version, source: path.resolve(values.source), output };
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
