import { chmod, copyFile, mkdir, mkdtemp, rename, rm } from "node:fs/promises";
import path from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { MACOS_TARGETS, UNIVERSAL_MACOS_TARGET } from "./build-config.mjs";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = path.resolve(appRoot, "../..");
const debug = process.argv.includes("--debug");
const profile = debug ? "debug" : "release";
const cargo = process.env.CARGO ?? "cargo";
const rustc = process.env.RUSTC ?? "rustc";
const cargoDirectory = path.dirname(cargo);
const pathValue = process.env.PATH ?? "";
const releaseVersion = process.env.DESKTOP_RELEASE_VERSION?.trim();
const buildEnv = {
  ...process.env,
  ...(path.isAbsolute(cargo) ? { PATH: `${cargoDirectory}${path.delimiter}${pathValue}` } : {}),
  ...(releaseVersion ? { PAP_BUILD_VERSION: releaseVersion } : {}),
};
const rustcOutput = execFileSync(rustc, ["-vV"], {
  cwd: repoRoot,
  encoding: "utf8",
  env: buildEnv,
});
const explicitTarget = process.env.PAP_BUILD_TARGET?.trim();
const targetTriple = explicitTarget || rustcOutput.match(/^host: (.+)$/m)?.[1];
if (!targetTriple) {
  throw new Error("Cannot determine the Rust host target triple");
}
const universal = targetTriple === UNIVERSAL_MACOS_TARGET;
if (universal && process.platform !== "darwin") throw new Error("Universal macOS builds require macOS and Xcode");
const targets = universal ? MACOS_TARGETS : [targetTriple];

const destinationDir = path.join(appRoot, "src-tauri/binaries");
await mkdir(destinationDir, { recursive: true });

// Executables embedded by the Tauri shell. The helper remains a console
// process so credential commands work on Windows.
const sidecars = [
  { name: "private-ai-proxy", manifestPath: path.join(repoRoot, "Cargo.toml") },
  {
    name: "private-ai-proxy-service",
    manifestPath: path.join(repoRoot, "Cargo.toml"),
  },
  {
    name: "private-ai-proxy-helper",
    manifestPath: path.join(appRoot, "gateway/Cargo.toml"),
  },
];

for (const sidecar of sidecars) {
  for (const target of targets) {
    const buildArgs = ["build", "--locked", "--manifest-path", sidecar.manifestPath, "--bin", sidecar.name];
    if (explicitTarget) buildArgs.push("--target", target);
    if (sidecar.manifestPath === path.join(repoRoot, "Cargo.toml")) buildArgs.push("--features", "desktop-client");
    if (!debug) buildArgs.push("--release");
    const build = spawnSync(cargo, buildArgs, { cwd: repoRoot, env: buildEnv, stdio: "inherit" });
    if (build.error) throw build.error;
    if (build.status !== 0) throw new Error(`cargo build ${sidecar.name} (${target}) failed: ${build.status ?? "unknown"}`);
  }
  const executable = process.platform === "win32" ? `${sidecar.name}.exe` : sidecar.name;
  const metadata = JSON.parse(execFileSync(cargo, [
    "metadata", "--no-deps", "--format-version", "1", "--manifest-path", sidecar.manifestPath,
  ], { cwd: repoRoot, env: buildEnv, encoding: "utf8" }));
  const sources = targets.map((target) => path.join(metadata.target_directory, ...(explicitTarget ? [target] : []), profile, executable));
  const destinationName = process.platform === "win32"
    ? `${sidecar.name}-${targetTriple}.exe`
    : `${sidecar.name}-${targetTriple}`;
  const destination = path.join(destinationDir, destinationName);
  const scratch = await mkdtemp(path.join(destinationDir, ".stage-"));
  try {
    const staged = path.join(scratch, executable);
    if (universal) {
      execFileSync("xcrun", ["lipo", "-create", ...sources, "-output", staged], { stdio: "inherit" });
      execFileSync("xcrun", ["lipo", staged, "-verify_arch", "arm64", "x86_64"], { stdio: "inherit" });
      // tauri-build also needs each thin binary during its two Cargo builds.
      for (const [index, target] of targets.entries()) {
        const name = `${sidecar.name}-${target}`;
        const thin = path.join(scratch, name);
        await copyFile(sources[index], thin);
        await chmod(thin, 0o755);
        await rename(thin, path.join(destinationDir, name));
      }
    } else {
      await copyFile(sources[0], staged);
    }
    if (process.platform !== "win32") await chmod(staged, 0o755);
    await rename(staged, destination);
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
  console.log(`Bundled ${destination}`);
}
