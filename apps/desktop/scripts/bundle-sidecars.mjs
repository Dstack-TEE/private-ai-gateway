import { chmod, copyFile, mkdir } from "node:fs/promises";
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
const buildEnv = path.isAbsolute(cargo)
  ? { ...process.env, PATH: `${cargoDirectory}${path.delimiter}${pathValue}` }
  : process.env;
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
  { name: "aci", manifestPath: path.join(repoRoot, "Cargo.toml"), targetDir: path.join(repoRoot, "target") },
  {
    name: "private-ai-gateway-helper",
    manifestPath: path.join(appRoot, "gateway/Cargo.toml"),
    targetDir: path.join(appRoot, "gateway/target"),
  },
];

for (const sidecar of sidecars) {
  const buildArgs = ["build", "--locked", "--manifest-path", sidecar.manifestPath, "--bin", sidecar.name];
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
  const source = path.join(sidecar.targetDir, profile, executable);
  const destinationName = process.platform === "win32"
    ? `${sidecar.name}-${targetTriple}.exe`
    : `${sidecar.name}-${targetTriple}`;
  const destination = path.join(destinationDir, destinationName);
  await copyFile(source, destination);
  if (process.platform !== "win32") {
    await chmod(destination, 0o755);
  }
  console.log(`Bundled ${destination}`);
}
