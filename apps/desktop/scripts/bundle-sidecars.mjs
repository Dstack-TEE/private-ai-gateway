import { chmod, copyFile, mkdir, mkdtemp, rename, rm } from "node:fs/promises";
import path from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { MACOS_TARGETS, UNIVERSAL_MACOS_TARGET } from "./build-config.mjs";
import {
  distribution,
  MAC_APP_STORE_DISTRIBUTION,
  MAC_APP_STORE_SIDECARS,
  runtimeBuildVersion,
} from "./distribution.mjs";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const debug = process.argv.includes("--debug");
const profile = debug ? "debug" : "release";
const cargo = process.env.CARGO ?? "cargo";
const rustc = process.env.RUSTC ?? "rustc";
const cargoDirectory = path.dirname(cargo);
const pathValue = process.env.PATH ?? "";
const buildVersion = runtimeBuildVersion();
const buildEnv = {
  ...process.env,
  ...(path.isAbsolute(cargo) ? { PATH: `${cargoDirectory}${path.delimiter}${pathValue}` } : {}),
  ...(buildVersion ? { PAP_BUILD_VERSION: buildVersion } : {}),
};
const npm = process.platform === "win32" ? "npm.cmd" : "npm";
execFileSync(npm, ["run", "build:web"], { cwd: appRoot, env: buildEnv, stdio: "inherit" });
const rustcOutput = execFileSync(rustc, ["-vV"], {
  cwd: appRoot,
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
await rm(destinationDir, { recursive: true, force: true });
await mkdir(destinationDir, { recursive: true });

// Executables embedded by the Tauri shell. The helper remains a console
// process so credential commands work on Windows.
const appStore = distribution() === MAC_APP_STORE_DISTRIBUTION;
const directSidecars = [
  { name: "private-ai-proxy", package: "private-ai-proxy", appStoreFeatures: ["mac-app-store"] },
  { name: "private-ai-proxy-service", package: "private-ai-proxy", appStoreFeatures: ["mac-app-store"] },
  {
    name: "private-ai-proxy-helper",
    package: "private-ai-proxy-gateway",
    appStoreFeatures: [],
  },
];
const sidecars = appStore
  ? directSidecars.filter((sidecar) => MAC_APP_STORE_SIDECARS.includes(sidecar.name))
  : directSidecars;

for (const sidecar of sidecars) {
  for (const target of targets) {
    const buildArgs = ["build", "--locked", "--package", sidecar.package, "--bin", sidecar.name];
    if (appStore && sidecar.appStoreFeatures.length > 0) {
      buildArgs.push("--features", sidecar.appStoreFeatures.join(","));
    }
    if (explicitTarget) buildArgs.push("--target", target);
    if (!debug) buildArgs.push("--release");
    const build = spawnSync(cargo, buildArgs, { cwd: appRoot, env: buildEnv, stdio: "inherit" });
    if (build.error) throw build.error;
    if (build.status !== 0) throw new Error(`cargo build ${sidecar.name} (${target}) failed: ${build.status ?? "unknown"}`);
  }
  const executable = process.platform === "win32" ? `${sidecar.name}.exe` : sidecar.name;
  const metadata = JSON.parse(execFileSync(cargo, [
    "metadata", "--no-deps", "--format-version", "1",
  ], { cwd: appRoot, env: buildEnv, encoding: "utf8" }));
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
