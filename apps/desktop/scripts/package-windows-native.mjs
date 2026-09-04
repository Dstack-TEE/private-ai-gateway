import { copyFile, mkdir, rm } from "node:fs/promises";
import { execFileSync, spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

if (process.platform !== "win32") throw new Error("The native Windows package must be built on Windows");

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const project = path.join(appRoot, "native/windows/PrivateAIGateway.Windows/PrivateAIGateway.Windows.csproj");
const stage = path.join(appRoot, "release/native-windows/Private-AI-Gateway");
const zip = path.join(appRoot, "release/Private-AI-Gateway-native-windows-x64.zip");

const run = (command, args, cwd = appRoot) => {
  const result = spawnSync(command, args, { cwd, stdio: "inherit", env: process.env });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} exited with ${result.status ?? "unknown"}`);
};

run("npm.cmd", ["run", "prepare:brand"]);
run("npm.cmd", ["run", "prepare:native-assets"]);
run("node", ["scripts/bundle-native.mjs"]);
await rm(stage, { recursive: true, force: true });
await mkdir(stage, { recursive: true });
run("dotnet", ["publish", project, "-c", "Release", "-r", "win-x64", "--self-contained", "true", "-p:Platform=x64", "-o", stage]);

const target = execFileSync(process.env.RUSTC ?? "rustc", ["-vV"], { encoding: "utf8" }).match(/^host: (.+)$/m)?.[1];
if (!target) throw new Error("Cannot determine the Rust target triple");
for (const name of ["private-ai-gateway-desktop-service", "private-ai-gateway-helper", "aci"]) {
  await copyFile(path.join(appRoot, "src-tauri/binaries", `${name}-${target}.exe`), path.join(stage, `${name}.exe`));
}

await rm(zip, { force: true });
const source = `${stage.replaceAll("'", "''")}\\*`;
const destination = zip.replaceAll("'", "''");
run("powershell.exe", ["-NoProfile", "-Command", `Compress-Archive -Path '${source}' -DestinationPath '${destination}' -CompressionLevel Optimal`]);
console.log(`Packaged ${zip}`);
