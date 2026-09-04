import { chmod, copyFile, cp, mkdir, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { execFileSync, spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

if (process.platform !== "linux") throw new Error("The native Linux package must be built on Linux");

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const release = path.join(appRoot, "release");
const stageRoot = path.join(release, "native-linux/Private-AI-Gateway");
const debRoot = path.join(release, "native-linux/deb");
const installRoot = path.join(debRoot, "usr/lib/private-ai-gateway");
const version = JSON.parse(await readFile(path.join(appRoot, "package.json"), "utf8")).version;
const architecture = process.arch === "arm64" ? "arm64" : "amd64";

const run = (command, args, cwd = appRoot) => {
  const result = spawnSync(command, args, { cwd, stdio: "inherit", env: process.env });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} exited with ${result.status ?? "unknown"}`);
};

run("npm", ["run", "prepare:brand"]);
run("npm", ["run", "prepare:native-assets"]);
run("node", ["scripts/bundle-native.mjs"]);
run(process.env.CARGO ?? "cargo", ["build", "--locked", "--release", "--manifest-path", "native/linux/Cargo.toml"]);

const target = execFileSync(process.env.RUSTC ?? "rustc", ["-vV"], { encoding: "utf8" }).match(/^host: (.+)$/m)?.[1];
if (!target) throw new Error("Cannot determine the Rust target triple");

await rm(path.join(release, "native-linux"), { recursive: true, force: true });
await mkdir(stageRoot, { recursive: true });
await stageApplication(stageRoot, target);

const tarball = path.join(release, `Private-AI-Gateway-native-linux-${process.arch}.tar.gz`);
await rm(tarball, { force: true });
run("tar", ["-C", path.dirname(stageRoot), "-czf", tarball, path.basename(stageRoot)]);

await mkdir(path.join(debRoot, "DEBIAN"), { recursive: true });
await mkdir(path.join(debRoot, "usr/bin"), { recursive: true });
await mkdir(path.join(debRoot, "usr/share/applications"), { recursive: true });
await mkdir(path.join(debRoot, "usr/share/icons/hicolor/512x512/apps"), { recursive: true });
await stageApplication(installRoot, target);
await symlink("../lib/private-ai-gateway/private-ai-gateway", path.join(debRoot, "usr/bin/private-ai-gateway"));
await copyFile(path.join(appRoot, "src-tauri/icons/icon.png"), path.join(debRoot, "usr/share/icons/hicolor/512x512/apps/private-ai-gateway.png"));
await writeFile(path.join(debRoot, "usr/share/applications/private-ai-gateway.desktop"), desktopEntry());
await writeFile(path.join(debRoot, "DEBIAN/control"), controlFile(version, architecture));

const deb = path.join(release, `private-ai-gateway_${version}_${architecture}.deb`);
await rm(deb, { force: true });
run("dpkg-deb", ["--build", "--root-owner-group", debRoot, deb]);
console.log(`Packaged ${tarball}`);
console.log(`Packaged ${deb}`);

async function stageApplication(destination, targetTriple) {
  await mkdir(destination, { recursive: true });
  await copyExecutable(path.join(appRoot, "native/linux/target/release/private-ai-gateway"), path.join(destination, "private-ai-gateway"));
  for (const name of ["private-ai-gateway-desktop-service", "private-ai-gateway-helper", "aci"]) {
    await copyExecutable(path.join(appRoot, "src-tauri/binaries", `${name}-${targetTriple}`), path.join(destination, name));
  }
  await cp(path.join(appRoot, "native/.generated-assets"), path.join(destination, "Assets"), { recursive: true });
}

async function copyExecutable(source, destination) {
  await copyFile(source, destination);
  await chmod(destination, 0o755);
}

function desktopEntry() {
  return `[Desktop Entry]\nType=Application\nName=Private AI Gateway\nComment=Hardware-verified private AI gateway\nExec=private-ai-gateway\nIcon=private-ai-gateway\nTerminal=false\nCategories=Development;Utility;Security;\nStartupNotify=true\n`;
}

function controlFile(packageVersion, packageArchitecture) {
  return `Package: private-ai-gateway\nVersion: ${packageVersion}\nSection: utils\nPriority: optional\nArchitecture: ${packageArchitecture}\nDepends: libgtk-4-1 (>= 4.8), libadwaita-1-0 (>= 1.2), libdbus-1-3, libsecret-1-0\nMaintainer: Dstack <support@dstack.ai>\nDescription: Hardware-verified private AI gateway desktop client\n Native GTK4 and libadwaita client backed by the shared Rust gateway runtime.\n`;
}
