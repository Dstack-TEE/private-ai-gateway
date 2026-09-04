import { chmod, copyFile, cp, mkdir, rm, symlink, writeFile } from "node:fs/promises";
import { execFileSync, spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

if (process.platform !== "darwin") {
  throw new Error("The native macOS package must be built on macOS");
}

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const packageRoot = path.join(appRoot, "native/macos");
const releaseRoot = path.join(appRoot, "release/native-macos");
const appName = "Private AI Gateway";
const bundleId = "org.dstack.private-ai-gateway";
const appBundle = path.join(releaseRoot, `${appName}.app`);
const contents = path.join(appBundle, "Contents");
const macos = path.join(contents, "MacOS");
const resources = path.join(contents, "Resources");
const loginBundle = path.join(contents, "Library/LoginItems/Private AI Gateway Login Item.app");
const dmgStage = path.join(releaseRoot, "dmg");

const run = (command, args, cwd = appRoot) => {
  const result = spawnSync(command, args, { cwd, stdio: "inherit", env: process.env });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} exited with ${result.status ?? "unknown"}`);
};

run("npm", ["run", "prepare:brand"]);
run("npm", ["run", "prepare:native-assets"]);
run("npm", ["run", "prepare:macos-icon"]);
run("node", ["scripts/bundle-native.mjs"]);
run("swift", ["build", "-c", "release", "--package-path", packageRoot]);

const swiftBin = execFileSync("swift", ["build", "-c", "release", "--show-bin-path", "--package-path", packageRoot], {
  cwd: appRoot,
  encoding: "utf8",
}).trim();
const rustc = execFileSync(process.env.RUSTC ?? "rustc", ["-vV"], { encoding: "utf8" });
const target = rustc.match(/^host: (.+)$/m)?.[1];
if (!target) throw new Error("Cannot determine the Rust target triple");

await rm(appBundle, { recursive: true, force: true });
await mkdir(macos, { recursive: true });
await mkdir(resources, { recursive: true });
await mkdir(path.join(loginBundle, "Contents/MacOS"), { recursive: true });

await copyExecutable(path.join(swiftBin, "PrivateAIGatewayMac"), path.join(macos, appName));
for (const name of ["private-ai-gateway-desktop-service", "private-ai-gateway-helper", "aci"]) {
  await copyExecutable(
    path.join(appRoot, "src-tauri/binaries", `${name}-${target}`),
    path.join(macos, name),
  );
}
await copyExecutable(
  path.join(swiftBin, "PrivateAIGatewayLoginItem"),
  path.join(loginBundle, "Contents/MacOS/Private AI Gateway Login Item"),
);

await copyFile(path.join(appRoot, "src-tauri/icons/icon.icns"), path.join(resources, "icon.icns"));
await copyFile(path.join(appRoot, "src-tauri/icons/Assets.car"), path.join(resources, "Assets.car"));
for (const name of ["trayTemplate.png", "trayTemplate@2x.png", "trayTemplateProtected.png", "trayTemplateProtected@2x.png"]) {
  await copyFile(path.join(appRoot, "assets/tray", name), path.join(resources, name));
}
await cp(path.join(appRoot, "native/.generated-assets"), path.join(resources, "Assets"), { recursive: true });

await writeFile(path.join(contents, "Info.plist"), plist({
  CFBundleDisplayName: appName,
  CFBundleExecutable: appName,
  CFBundleIconFile: "icon.icns",
  CFBundleIconName: "Icon",
  CFBundleIdentifier: bundleId,
  CFBundleName: appName,
  CFBundlePackageType: "APPL",
  CFBundleShortVersionString: "0.1.0",
  CFBundleVersion: "1",
  LSApplicationCategoryType: "public.app-category.developer-tools",
  LSMinimumSystemVersion: "14.0",
  NSHighResolutionCapable: true,
}));
await writeFile(path.join(loginBundle, "Contents/Info.plist"), plist({
  CFBundleDisplayName: "Private AI Gateway Login Item",
  CFBundleExecutable: "Private AI Gateway Login Item",
  CFBundleIdentifier: `${bundleId}.login-item`,
  CFBundleName: "Private AI Gateway Login Item",
  CFBundlePackageType: "APPL",
  CFBundleShortVersionString: "0.1.0",
  CFBundleVersion: "1",
  LSBackgroundOnly: true,
  LSMinimumSystemVersion: "14.0",
}));

run("codesign", ["--force", "--deep", "--sign", "-", appBundle]);
await mkdir(path.join(appRoot, "release"), { recursive: true });
const zip = path.join(appRoot, "release/Private-AI-Gateway-native-macos.zip");
await rm(zip, { force: true });
run("ditto", ["-c", "-k", "--sequesterRsrc", "--keepParent", appBundle, zip]);
const dmg = path.join(appRoot, "release/Private-AI-Gateway-native-macos.dmg");
await rm(dmgStage, { recursive: true, force: true });
await mkdir(dmgStage, { recursive: true });
await cp(appBundle, path.join(dmgStage, `${appName}.app`), { recursive: true });
await symlink("/Applications", path.join(dmgStage, "Applications"));
await rm(dmg, { force: true });
run("hdiutil", ["create", "-volname", appName, "-srcfolder", dmgStage, "-ov", "-format", "UDZO", dmg]);
await rm(dmgStage, { recursive: true, force: true });
console.log(`Packaged ${appBundle}`);
console.log(`Packaged ${zip}`);
console.log(`Packaged ${dmg}`);

async function copyExecutable(source, destination) {
  await copyFile(source, destination);
  await chmod(destination, 0o755);
}

function plist(values) {
  const entries = Object.entries(values).map(([key, value]) => `  <key>${key}</key>\n  ${plistValue(value)}`).join("\n");
  return `<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0">\n<dict>\n${entries}\n</dict>\n</plist>\n`;
}

function plistValue(value) {
  if (typeof value === "boolean") return value ? "<true/>" : "<false/>";
  return `<string>${String(value).replaceAll("&", "&amp;").replaceAll("<", "&lt;")}</string>`;
}
