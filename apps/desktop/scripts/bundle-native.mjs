import { chmod, copyFile, mkdir } from "node:fs/promises";
import path from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = path.resolve(appRoot, "../..");
const debug = process.argv.includes("--debug");
const profile = debug ? "debug" : "release";
const executableName = process.platform === "win32" ? "aci.exe" : "aci";
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

const buildArgs = ["build", "--bin", "aci"];
if (!debug) {
  buildArgs.push("--release");
}
const build = spawnSync(cargo, buildArgs, {
  cwd: repoRoot,
  env: buildEnv,
  stdio: "inherit",
});

if (build.error) {
  throw build.error;
}
if (build.status !== 0) {
  throw new Error(`cargo build exited with status ${build.status ?? "unknown"}`);
}

const destinationDir = path.join(appRoot, "src-tauri/binaries");
const source = path.join(repoRoot, "target", profile, executableName);
const destinationName = process.platform === "win32"
  ? `aci-${targetTriple}.exe`
  : `aci-${targetTriple}`;
const destination = path.join(destinationDir, destinationName);
await mkdir(destinationDir, { recursive: true });
await copyFile(source, destination);
if (process.platform !== "win32") {
  await chmod(destination, 0o755);
}

console.log(`Bundled ${destination}`);
