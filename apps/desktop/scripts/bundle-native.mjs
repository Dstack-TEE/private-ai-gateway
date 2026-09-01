import { chmod, copyFile, mkdir } from "node:fs/promises";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = path.resolve(appRoot, "../..");
const executableName = process.platform === "win32" ? "aci.exe" : "aci";
const cargo = process.env.CARGO ?? "cargo";
const cargoDirectory = path.dirname(cargo);
const pathValue = process.env.PATH ?? "";
const buildEnv = path.isAbsolute(cargo)
  ? { ...process.env, PATH: `${cargoDirectory}${path.delimiter}${pathValue}` }
  : process.env;
const build = spawnSync(cargo, ["build", "--release", "--bin", "aci"], {
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

const destinationDir = path.join(
  appRoot,
  "resources/native",
  `${process.platform}-${process.arch}`,
);
const source = path.join(repoRoot, "target/release", executableName);
const destination = path.join(destinationDir, executableName);
await mkdir(destinationDir, { recursive: true });
await copyFile(source, destination);
if (process.platform !== "win32") {
  await chmod(destination, 0o755);
}

console.log(`Bundled ${destination}`);
