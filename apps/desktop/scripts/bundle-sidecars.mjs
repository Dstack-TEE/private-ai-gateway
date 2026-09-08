import { chmod, copyFile, mkdir, mkdtemp, rename, rm } from "node:fs/promises";
import path from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

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
  ...(releaseVersion ? { PAG_BUILD_VERSION: releaseVersion } : {}),
};
const rustcOutput = execFileSync(rustc, ["-vV"], {
  cwd: repoRoot,
  encoding: "utf8",
  env: buildEnv,
});
const targetTriple = rustcOutput.match(/^host: (.+)$/m)?.[1];
if (!targetTriple) {
  throw new Error("Cannot determine the Rust host target triple");
}

const destinationDir = path.join(appRoot, "src-tauri/binaries");
await mkdir(destinationDir, { recursive: true });

// Executables embedded by the Tauri shell. The helper remains a console
// process so credential commands work on Windows.
const sidecars = [
  { name: "pap", manifestPath: path.join(repoRoot, "Cargo.toml") },
  {
    name: "pag",
    manifestPath: path.join(appRoot, "runtime/Cargo.toml"),
  },
  {
    name: "pag-service",
    manifestPath: path.join(appRoot, "runtime/Cargo.toml"),
  },
  {
    name: "private-ai-gateway-helper",
    manifestPath: path.join(appRoot, "gateway/Cargo.toml"),
  },
];

for (const sidecar of sidecars) {
  const buildArgs = ["build", "--locked", "--manifest-path", sidecar.manifestPath, "--bin", sidecar.name];
  if (sidecar.name === "pap") buildArgs.push("--features", "desktop-client");
  if (!debug) {
    buildArgs.push("--release");
  }
  const build = spawnSync(cargo, buildArgs, { cwd: repoRoot, env: buildEnv, stdio: "inherit" });
  if (build.error) {
    throw build.error;
  }
  if (build.status !== 0) {
    throw new Error(`cargo build ${sidecar.name} exited with status ${build.status ?? "unknown"}`);
  }
  const executable = process.platform === "win32" ? `${sidecar.name}.exe` : sidecar.name;
  const metadata = JSON.parse(execFileSync(cargo, [
    "metadata", "--no-deps", "--format-version", "1", "--manifest-path", sidecar.manifestPath,
  ], { cwd: repoRoot, env: buildEnv, encoding: "utf8" }));
  const source = path.join(metadata.target_directory, profile, executable);
  const destinationName = process.platform === "win32"
    ? `${sidecar.name}-${targetTriple}.exe`
    : `${sidecar.name}-${targetTriple}`;
  const destination = path.join(destinationDir, destinationName);
  const scratch = await mkdtemp(path.join(destinationDir, ".stage-"));
  try {
    const staged = path.join(scratch, executable);
    await copyFile(source, staged);
    if (process.platform !== "win32") await chmod(staged, 0o755);
    await rename(staged, destination);
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
  console.log(`Bundled ${destination}`);
}
