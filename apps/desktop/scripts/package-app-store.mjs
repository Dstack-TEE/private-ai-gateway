#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { chmod, copyFile, mkdir, mkdtemp, readFile, readdir, open, rm, lstat, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { MAC_APP_STORE_SIDECARS, validateAppStoreBuildNumber } from "./distribution.mjs";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

export function appStoreRuntimeEntitlements() {
  return {
    main: {
      "com.apple.security.app-sandbox": true,
      "com.apple.security.network.client": true,
      "com.apple.security.network.server": true,
      "com.apple.security.files.user-selected.read-write": true,
      "com.apple.security.files.bookmarks.app-scope": true,
    },
    child: {
      "com.apple.security.app-sandbox": true,
      "com.apple.security.inherit": true,
    },
  };
}

export function appStoreEntitlements(profile, bundleIdentifier) {
  const entitlements = profile?.Entitlements;
  const appIdentifier = entitlements?.["com.apple.application-identifier"];
  const teamIdentifier = entitlements?.["com.apple.developer.team-identifier"] ?? profile?.TeamIdentifier?.[0];
  if (typeof appIdentifier !== "string" || !appIdentifier.endsWith(`.${bundleIdentifier}`)) {
    throw new Error(`Provisioning profile does not allow ${bundleIdentifier}`);
  }
  if (typeof teamIdentifier !== "string" || !teamIdentifier) {
    throw new Error("Provisioning profile does not contain a team identifier");
  }
  if (entitlements?.["get-task-allow"] === true || entitlements?.["com.apple.security.get-task-allow"] === true) {
    throw new Error("A development provisioning profile cannot be used for App Store packaging");
  }
  if (Array.isArray(profile.ProvisionedDevices) || profile.ProvisionsAllDevices === true) {
    throw new Error("Provisioning profile is not a Mac App Store Connect distribution profile");
  }
  const allowedGroups = entitlements?.["keychain-access-groups"];
  if (!Array.isArray(allowedGroups) || !allowedGroups.some((group) => group === appIdentifier || (typeof group === "string" && group.endsWith(".*") && appIdentifier.startsWith(group.slice(0, -1))))) {
    throw new Error("Provisioning profile does not allow the application Keychain group");
  }
  const expiration = Date.parse(profile.ExpirationDate);
  if (!Number.isFinite(expiration) || expiration <= Date.now()) {
    throw new Error("Provisioning profile expiry is missing, invalid, or expired");
  }
  if (!profile.TeamIdentifier?.includes(teamIdentifier)) {
    throw new Error("Provisioning profile team identifiers do not match");
  }
  const runtime = appStoreRuntimeEntitlements();
  return {
    main: {
      ...runtime.main,
      "com.apple.application-identifier": appIdentifier,
      "com.apple.developer.team-identifier": teamIdentifier,
      "keychain-access-groups": [appIdentifier],
    },
    child: runtime.child,
  };
}

export function validateAppStoreManifest(info, bundleIdentifier) {
  if (info.CFBundleIdentifier !== bundleIdentifier) throw new Error("Application bundle identifier does not match the provisioning profile");
  if (!/^[0-9]+\.[0-9]+\.[0-9]+$/.test(info.CFBundleShortVersionString ?? "")) throw new Error("A stable marketing version is required");
  validateAppStoreBuildNumber(info.CFBundleVersion);
  if (!/^\d+(?:\.\d+){0,2}$/.test(info.LSMinimumSystemVersion ?? "") || Number(info.LSMinimumSystemVersion.split(".")[0]) < 13) throw new Error("macOS 13 or newer is required for SMAppService");
  if (info.LSApplicationCategoryType !== "public.app-category.developer-tools") throw new Error("App Store category must be Developer Tools");
  if (typeof info.CFBundleExecutable !== "string" || !/^[A-Za-z0-9._-]+$/.test(info.CFBundleExecutable) || info.CFBundleExecutable === "." || info.CFBundleExecutable === "..") throw new Error("Invalid application executable name");
  return [info.CFBundleExecutable, ...MAC_APP_STORE_SIDECARS];
}

// Fail closed if a new executable is added without a signing policy. This
// includes nested helpers, frameworks and scripts outside Contents/MacOS.
async function validateBundledCode(app, names) {
  const allowed = new Set(names.map((name) => path.join(app, "Contents/MacOS", name)));
  for (const entry of await readdir(app, { recursive: true, withFileTypes: true })) {
    if (entry.isDirectory()) continue;
    const file = path.join(entry.parentPath, entry.name);
    if (entry.isSymbolicLink()) throw new Error(`Unreviewed bundle symlink: ${file}`);
    if (!entry.isFile()) throw new Error(`Unexpected bundle entry: ${file}`);
    const handle = await open(file, "r");
    try {
      const header = Buffer.alloc(4);
      const { bytesRead } = await handle.read(header, 0, 4, 0);
      const magic = bytesRead === 4 ? header.readUInt32BE() : 0;
      const code = ((await handle.stat()).mode & 0o111) !== 0 || [0xfeedface, 0xcefaedfe, 0xfeedfacf, 0xcffaedfe, 0xcafebabe, 0xbebafeca, 0xcafebabf, 0xbfbafeca].includes(magic) || header.subarray(0, 2).toString() === "#!";
      if (code && !allowed.has(file)) throw new Error(`Unreviewed bundled executable: ${file}`);
    } finally {
      await handle.close();
    }
  }
}

function argumentsFrom(argv) {
  const values = new Map();
  for (let index = 0; index < argv.length; index += 2) {
    const key = argv[index];
    const value = argv[index + 1];
    if (!key?.startsWith("--") || !value) throw new Error(`Invalid argument ${key ?? ""}`.trim());
    values.set(key.slice(2), value);
  }
  const required = (name) => {
    const value = values.get(name)?.trim();
    if (!value) throw new Error(`--${name} is required`);
    return value;
  };
  return {
    app: path.resolve(required("app")),
    profile: path.resolve(required("profile")),
    applicationIdentity: required("application-identity"),
    installerIdentity: required("installer-identity"),
    output: path.resolve(required("output")),
  };
}

function run(command, args, options = {}) {
  return execFileSync(command, args, { encoding: "utf8", stdio: options.capture ? "pipe" : "inherit" });
}

export function readProvisioningProfile(file) {
  // Profiles contain plist Data (certificates) and Date values, which cannot
  // be converted wholesale to JSON by plutil. Read only the policy fields.
  return JSON.parse(run("python3", ["-c", [
    "import json, plistlib, sys",
    "with open(sys.argv[1], 'rb') as source: profile = plistlib.load(source)",
    "keys = ('Entitlements', 'TeamIdentifier', 'ExpirationDate', 'ProvisionedDevices', 'ProvisionsAllDevices')",
    "print(json.dumps({key: profile[key] for key in keys if key in profile}, default=lambda value: value.isoformat() + 'Z'))",
  ].join("\n"), file], { capture: true }));
}

async function requireFile(file, description) {
  if (!(await lstat(file).catch(() => undefined))?.isFile()) {
    throw new Error(`${description} is missing: ${file}`);
  }
}

async function writePlist(file, value) {
  await writeFile(file, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 });
  run("plutil", ["-convert", "xml1", file]);
}

export async function embedProvisioningProfile(profile, app) {
  const destination = path.join(app, "Contents/embedded.provisionprofile");
  await copyFile(profile, destination);
  await chmod(destination, 0o644);
}

async function main() {
  if (process.platform !== "darwin") throw new Error("Mac App Store packages must be built on macOS");
  const options = argumentsFrom(process.argv.slice(2));
  const { identifier } = JSON.parse(await readFile(path.join(appRoot, "src-tauri/tauri.conf.json"), "utf8"));
  await requireFile(path.join(options.app, "Contents/Info.plist"), "Application Info.plist");
  await requireFile(options.profile, "Mac App Store provisioning profile");
  const scratch = await mkdtemp(path.join(os.tmpdir(), "private-ai-proxy-app-store-"));
  try {
    const decodedProfile = path.join(scratch, "profile.plist");
    run("security", ["cms", "-D", "-i", options.profile, "-o", decodedProfile]);
    const profile = readProvisioningProfile(decodedProfile);
    const entitlements = appStoreEntitlements(profile, identifier);
    const mainEntitlements = path.join(scratch, "main.entitlements");
    const childEntitlements = path.join(scratch, "child.entitlements");
    await writePlist(mainEntitlements, entitlements.main);
    await writePlist(childEntitlements, entitlements.child);

    await embedProvisioningProfile(options.profile, options.app);
    const executableDirectory = path.join(options.app, "Contents/MacOS");
    const infoPlist = path.join(options.app, "Contents/Info.plist");
    const info = JSON.parse(run("plutil", ["-convert", "json", "-o", "-", infoPlist], { capture: true }));
    const executables = validateAppStoreManifest(info, identifier);
    await validateBundledCode(options.app, executables);
    const mainExecutable = info.CFBundleExecutable;
    const children = MAC_APP_STORE_SIDECARS;
    for (const name of [mainExecutable, ...children]) {
      const executable = path.join(executableDirectory, name);
      await requireFile(executable, `Bundled ${name} executable`);
      const architectures = new Set(run("xcrun", ["lipo", "-archs", executable], { capture: true }).trim().split(/\s+/));
      if (!architectures.has("arm64") || !architectures.has("x86_64")) {
        throw new Error(`${name} is not a Universal macOS executable`);
      }
    }
    for (const name of children) {
      const executable = path.join(executableDirectory, name);
      run("codesign", ["--force", "--sign", options.applicationIdentity, "--entitlements", childEntitlements, "--timestamp", executable]);
    }
    run("codesign", ["--force", "--sign", options.applicationIdentity, "--entitlements", mainEntitlements, "--timestamp", options.app]);
    for (const name of [mainExecutable, ...children]) {
      run("codesign", ["--verify", "--strict", "--verbose=2", path.join(executableDirectory, name)]);
    }
    run("codesign", ["--verify", "--deep", "--strict", "--verbose=2", options.app]);

    await mkdir(path.dirname(options.output), { recursive: true });
    await rm(options.output, { force: true });
    run("xcrun", ["productbuild", "--component", options.app, "/Applications", "--sign", options.installerIdentity, options.output]);
    run("pkgutil", ["--check-signature", options.output]);
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main();
}
