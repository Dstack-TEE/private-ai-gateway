import assert from "node:assert/strict";
import { test } from "node:test";
import { releaseConfig } from "./release-config.mjs";

const beta = "https://github.com/o/r/releases/download/desktop-updates-beta/latest.json";
const updater = { TAURI_UPDATER_PUBLIC_KEY: "key", TAURI_UPDATER_ENDPOINT: beta };

test("test builds carry no release settings", () => {
  assert.deepEqual(releaseConfig("direct", {}, "0.2.0-beta.0"), {});
});

test("updater builds point at their own channel's feed over HTTPS", () => {
  assert.deepEqual(releaseConfig("direct", updater, "0.2.0-beta.1"), {
    plugins: { updater: { pubkey: "key", endpoints: [beta] } },
    bundle: { createUpdaterArtifacts: true },
  });
  assert.throws(() => releaseConfig("direct", updater, "0.2.0"), /does not match the release channel/);
  assert.throws(() => releaseConfig("direct", updater, "0.2.0-beta.0"), /beta\.n/);
  assert.throws(() => releaseConfig("direct", { TAURI_UPDATER_PUBLIC_KEY: "key" }, "0.2.0-beta.1"), /or neither/);
  for (const endpoint of [beta.replace("https:", "http:"), beta.replace("https://", "https://user:secret@")]) {
    assert.throws(() => releaseConfig("direct", { ...updater, TAURI_UPDATER_ENDPOINT: endpoint }, "0.2.0-beta.1"), /HTTPS/);
  }
});

test("App Store builds take a build number only for stable versions and never the updater", () => {
  assert.deepEqual(releaseConfig("mac-app-store", { APPLE_APP_STORE_BUILD_NUMBER: "104" }, "0.2.0"), {
    bundle: { macOS: { bundleVersion: "104" } },
  });
  assert.throws(() => releaseConfig("mac-app-store", { APPLE_APP_STORE_BUILD_NUMBER: "104" }, "0.2.0-beta.1"), /stable version/);
  assert.throws(() => releaseConfig("mac-app-store", { APPLE_APP_STORE_BUILD_NUMBER: "1.2.3.4" }, "0.2.0"), /build number/);
  assert.throws(() => releaseConfig("direct", { APPLE_APP_STORE_BUILD_NUMBER: "104" }, "0.2.0"), /only valid for Mac App Store/);
  assert.throws(() => releaseConfig("mac-app-store", updater, "0.2.0"), /cannot include the native updater/);
});

test("Windows signing takes a SHA-1 thumbprint", () => {
  const thumbprint = "a".repeat(40);
  assert.deepEqual(releaseConfig("direct", { WINDOWS_CERTIFICATE_THUMBPRINT: thumbprint }, "0.2.0").bundle.windows, {
    certificateThumbprint: thumbprint, digestAlgorithm: "sha256", timestampUrl: "http://timestamp.digicert.com",
  });
  assert.throws(() => releaseConfig("direct", { WINDOWS_CERTIFICATE_THUMBPRINT: "a".repeat(39) }, "0.2.0"), /thumbprint/);
});
