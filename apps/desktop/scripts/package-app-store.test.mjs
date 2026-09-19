import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { appStoreEntitlements, readProvisioningProfile, validateAppStoreManifest } from "./package-app-store.mjs";
import { MAC_APP_STORE_SIDECARS } from "./distribution.mjs";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

test("reads a real plist Date without attempting to JSON-encode certificate Data", { skip: process.platform === "win32" }, async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "pap-profile-test-"));
  try {
    const file = path.join(directory, "profile.plist");
    await writeFile(file, `<?xml version="1.0"?><plist version="1.0"><dict>
      <key>ExpirationDate</key><date>2099-01-01T00:00:00Z</date>
      <key>DeveloperCertificates</key><array><data>AQID</data></array>
      <key>TeamIdentifier</key><array><string>TEAM123</string></array>
      <key>Entitlements</key><dict><key>get-task-allow</key><false/></dict>
    </dict></plist>`);
    assert.deepEqual(readProvisioningProfile(file), {
      ExpirationDate: "2099-01-01T00:00:00Z",
      TeamIdentifier: ["TEAM123"],
      Entitlements: { "get-task-allow": false },
    });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

const profile = {
  ExpirationDate: "2099-01-01T00:00:00Z",
  TeamIdentifier: ["TEAM123"],
  Entitlements: {
    "com.apple.application-identifier": "TEAM123.org.dstack.private-ai-proxy",
    "com.apple.developer.team-identifier": "TEAM123",
    "keychain-access-groups": ["TEAM123.*"],
    "get-task-allow": false,
  },
};

test("derives separate main and child App Sandbox entitlements", () => {
  const result = appStoreEntitlements(profile, "org.dstack.private-ai-proxy");
  assert.equal(result.main["com.apple.security.network.server"], true);
  assert.equal(result.main["com.apple.security.files.bookmarks.app-scope"], true);
  assert.deepEqual(result.main["keychain-access-groups"], ["TEAM123.org.dstack.private-ai-proxy"]);
  assert.deepEqual(result.child, {
    "com.apple.security.app-sandbox": true,
    "com.apple.security.inherit": true,
  });
});

test("rejects profiles for another application or development", () => {
  assert.throws(() => appStoreEntitlements(profile, "org.example.other"), /does not allow/);
  assert.throws(() => appStoreEntitlements({ ...profile, Entitlements: { ...profile.Entitlements, "get-task-allow": true } }, "org.dstack.private-ai-proxy"), /development/);
  assert.throws(() => appStoreEntitlements({ ...profile, Entitlements: { ...profile.Entitlements, "com.apple.security.get-task-allow": true } }, "org.dstack.private-ai-proxy"), /development/);
  assert.throws(() => appStoreEntitlements({ ...profile, ProvisionedDevices: ["device"] }, "org.dstack.private-ai-proxy"), /Mac App Store Connect/);
  assert.throws(() => appStoreEntitlements({ ...profile, ProvisionsAllDevices: true }, "org.dstack.private-ai-proxy"), /Mac App Store Connect/);
});

test("App Store configuration embeds the required runtime executables", async () => {
  const config = JSON.parse(await readFile(path.join(appRoot, "src-tauri/tauri.appstore.conf.json"), "utf8"));
  assert.deepEqual(
    config.bundle.externalBin,
    MAC_APP_STORE_SIDECARS.map((name) => `binaries/${name}`),
  );
});

const manifest = {
  CFBundleIdentifier: "org.dstack.private-ai-proxy",
  CFBundleExecutable: "private-ai-proxy-desktop",
  CFBundleShortVersionString: "1.2.3",
  CFBundleVersion: "42.1",
  LSMinimumSystemVersion: "13.0",
  LSApplicationCategoryType: "public.app-category.developer-tools",
};

test("package manifest requires a stable Universal-era app and valid build version", () => {
  const identifier = manifest.CFBundleIdentifier;
  assert.deepEqual(validateAppStoreManifest(manifest, identifier), [manifest.CFBundleExecutable, ...MAC_APP_STORE_SIDECARS]);
  for (const patch of [
    { CFBundleVersion: "2026091901" }, { CFBundleVersion: "1.100" },
    { CFBundleShortVersionString: "1.2.3-beta.1" }, { LSMinimumSystemVersion: "12.0" },
    { CFBundleIdentifier: "com.invalid.desktop-app" }, { CFBundleExecutable: "../outside" },
  ]) assert.throws(() => validateAppStoreManifest({ ...manifest, ...patch }, identifier));
  for (const ExpirationDate of [undefined, "invalid", "2000-01-01"]) {
    assert.throws(() => appStoreEntitlements({ ...profile, ExpirationDate }, identifier), /expiry/);
  }
});

test("MAS config disables updater packaging and external credential helpers", async () => {
  const config = JSON.parse(await readFile(path.join(appRoot, "src-tauri/tauri.appstore.conf.json"), "utf8"));
  assert.equal(config.plugins.updater, null);
  assert.equal(config.bundle.createUpdaterArtifacts, false);
  assert.deepEqual(config.bundle.targets, ["app"]);
  assert.deepEqual(config.build.features, ["mac-app-store"]);
  assert(!MAC_APP_STORE_SIDECARS.includes("private-ai-proxy-helper"));
});
