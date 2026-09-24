#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { artifactName } from "./release-artifacts.mjs";
import { UNIVERSAL_MACOS_TARGET } from "./build-config.mjs";
import { aliases, aliasLinks, buildLinuxPackages, releaseVersionParts } from "./package-linux.mjs";
import { chmod, copyFile, mkdir, mkdtemp, rm, stat, symlink } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const binaries = ["private-ai-proxy", "private-ai-proxy-service", "private-ai-proxy-helper"];

const scriptPath = fileURLToPath(import.meta.url);
const appRoot = path.resolve(path.dirname(scriptPath), "..");

export async function assertWebBundle(root = appRoot) {
  const index = path.join(root, "runtime/web-dist/index.html");
  if (!(await stat(index).catch(() => undefined))?.isFile()) {
    throw new Error(`Missing embedded web UI ${index}; run npm run build:web before building or packaging the CLI`);
  }
}

export async function stagePortable({ sourceDir, targetTriple, platform, destination }) {
  await mkdir(destination, { recursive: true });
  for (const name of binaries) {
    const extension = platform === "windows" ? ".exe" : "";
    const staged = path.join(sourceDir, `${name}-${targetTriple}${extension}`);
    const source = (await stat(staged).catch(() => undefined))?.isFile()
      ? staged : path.join(sourceDir, `${name}${extension}`);
    const target = path.join(destination, `${name}${extension}`);
    const metadata = await stat(source).catch(() => undefined);
    if (!metadata?.isFile()) {
      throw new Error(`Missing staged CLI binary ${source}`);
    }
    await copyFile(source, target);
    if (targetTriple === UNIVERSAL_MACOS_TARGET) {
      execFileSync("xcrun", ["lipo", target, "-verify_arch", "arm64", "x86_64"], { stdio: "inherit" });
    }
    if (platform !== "windows") {
      await chmod(target, 0o755);
    }
  }
  if (platform === "windows") {
    for (const alias of aliases) {
      // The shim `pap cli install` writes (cli/manage/install.rs).
      await copyFile(path.join(appRoot, "cli/manage/alias.cmd"), path.join(destination, `${alias}.cmd`));
    }
  } else {
    for (const alias of aliases) {
      await symlink("private-ai-proxy", path.join(destination, alias));
    }
  }
}

// Linux packages keep the executables in libexec behind a /usr/bin symlink;
// the CLI canonicalizes itself before it locates its siblings.
export function linuxContents(portable) {
  const libexec = "/usr/libexec/private-ai-proxy";
  return [
    { dst: libexec, type: "dir" },
    ...binaries.map((name) => ({ src: path.join(portable, name), dst: `${libexec}/${name}`, file_info: { mode: 0o755 } })),
    { src: "../libexec/private-ai-proxy/private-ai-proxy", dst: "/usr/bin/private-ai-proxy", type: "symlink" },
    ...aliasLinks,
    // systemd.unit(5): packages install user units in /usr/lib/systemd/user.
    { src: path.join(appRoot, "src-tauri/installer/private-ai-proxy.service"), dst: "/usr/lib/systemd/user/private-ai-proxy.service", file_info: { mode: 0o644 } },
  ];
}

async function main() {
  await assertWebBundle();
  const options = parseArguments(process.argv.slice(2));
  await mkdir(options.output, { recursive: true });
  const scratch = await mkdtemp(path.join(options.output, ".pap-cli-"));
  const artifactBase = artifactName({ ...options, cli: true });
  const portable = path.join(scratch, artifactBase);
  const artifacts = [];

  try {
    await stagePortable({
      sourceDir: options.source,
      targetTriple: options.targetTriple,
      platform: options.platform,
      destination: portable,
    });
    const archive = path.join(
      options.output,
      `${artifactBase}.${options.platform === "windows" ? "zip" : "tar.gz"}`,
    );
    createArchive(options.platform, scratch, artifactBase, archive);
    artifacts.push(archive);

    if (options.platform === "linux") {
      artifacts.push(...await buildLinuxPackages({
        kind: "cli", version: options.version, arch: options.arch, contents: linuxContents(portable), output: options.output,
      }));
    }

    for (const artifact of artifacts) console.log(`Packaged ${artifact}`);
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
}

function parseArguments(arguments_) {
  const values = new Map();
  for (let index = 0; index < arguments_.length; index += 2) {
    const key = arguments_[index];
    const value = arguments_[index + 1];
    if (!key?.startsWith("--") || value === undefined) {
      throw new Error("Usage: package-cli.mjs --platform <windows|macos|linux> --arch <x64|arm64|universal> --version <semver> --target-triple <triple> [--source <dir>] [--output <dir>]");
    }
    values.set(key.slice(2), value);
  }
  const platform = values.get("platform");
  const arch = values.get("arch");
  const version = values.get("version");
  const targetTriple = values.get("target-triple");
  if (!["windows", "macos", "linux"].includes(platform)) {
    throw new Error(`Unsupported CLI package platform ${JSON.stringify(platform)}`);
  }
  if (!["x64", "arm64", "universal"].includes(arch)) {
    throw new Error(`Unsupported CLI package architecture ${JSON.stringify(arch)}`);
  }
  if (!targetTriple || !/^[A-Za-z0-9_.-]+$/.test(targetTriple)) {
    throw new Error("A valid --target-triple is required");
  }
  const targetArch = targetTriple === UNIVERSAL_MACOS_TARGET ? "universal" : targetTriple.startsWith("x86_64-") ? "x64" : targetTriple.startsWith("aarch64-") ? "arm64" : undefined;
  const targetPlatform = targetTriple.endsWith("apple-darwin") ? "macos" : targetTriple.includes("-windows-") ? "windows" : targetTriple.includes("-linux-") ? "linux" : undefined;
  releaseVersionParts(version);
  if (platform !== targetPlatform || arch !== targetArch) {
    throw new Error("CLI package platform or architecture does not match the Rust target triple");
  }
  return {
    platform,
    arch,
    version,
    targetTriple,
    source: path.resolve(values.get("source") ?? path.join(appRoot, "src-tauri/binaries")),
    output: path.resolve(values.get("output") ?? path.join(appRoot, "release")),
  };
}

function createArchive(platform, parent, directory, output) {
  if (platform === "windows") {
    const systemRoot = process.env.SystemRoot ?? process.env.SYSTEMROOT;
    if (!systemRoot || !path.isAbsolute(systemRoot)) throw new Error("SystemRoot is required for the native Windows archive tool");
    execFileSync(path.join(systemRoot, "System32", "tar.exe"), ["-a", "-c", "-f", output, "-C", parent, directory], { stdio: "inherit" });
  } else {
    execFileSync("tar", ["-c", "-z", "-f", output, "-C", parent, directory], { stdio: "inherit" });
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === scriptPath) {
  await main();
}
