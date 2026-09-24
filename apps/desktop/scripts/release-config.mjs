import { appVersion, versionRelease } from "./release-channel.mjs";
import { MAC_APP_STORE_DISTRIBUTION, validateAppStoreBuildNumber } from "./distribution.mjs";

// Release-only Tauri settings from the CI environment, merged by
// `tauri build --config <JSON>` over the committed tauri.conf.json.
export function releaseConfig(buildDistribution, env = process.env) {
  const appStoreBuildNumber = env.APPLE_APP_STORE_BUILD_NUMBER?.trim();
  const updaterKey = env.TAURI_UPDATER_PUBLIC_KEY?.trim();
  const updaterEndpoint = env.TAURI_UPDATER_ENDPOINT?.trim();
  const windowsCertificateThumbprint = env.WINDOWS_CERTIFICATE_THUMBPRINT?.trim();
  const appStore = buildDistribution === MAC_APP_STORE_DISTRIBUTION;
  if (appStore && (updaterKey || updaterEndpoint)) {
    throw new Error("Mac App Store builds cannot include the native updater");
  }
  if (appStoreBuildNumber) {
    if (!appStore) throw new Error("APPLE_APP_STORE_BUILD_NUMBER is only valid for Mac App Store builds");
    validateAppStoreBuildNumber(appStoreBuildNumber);
    if (versionRelease(appVersion()).channel !== "stable") throw new Error("Mac App Store releases must use a stable version");
  }
  if (Boolean(updaterKey) !== Boolean(updaterEndpoint)) {
    throw new Error("Set both TAURI_UPDATER_PUBLIC_KEY and TAURI_UPDATER_ENDPOINT, or neither");
  }
  if (updaterEndpoint) {
    const endpoint = new URL(updaterEndpoint);
    if (!endpoint.pathname.endsWith(`/${versionRelease(appVersion()).feedTag}/latest.json`)) {
      throw new Error("Updater endpoint does not match the release channel");
    }
    if (endpoint.protocol !== "https:" || endpoint.username || endpoint.password) {
      throw new Error("The updater endpoint must use HTTPS without embedded credentials");
    }
  }
  if (windowsCertificateThumbprint && !/^[0-9a-f]{40}$/i.test(windowsCertificateThumbprint)) {
    throw new Error("WINDOWS_CERTIFICATE_THUMBPRINT must be a SHA-1 certificate thumbprint");
  }
  const config = {};
  if (updaterKey) {
    config.plugins = { updater: { pubkey: updaterKey, endpoints: [updaterEndpoint] } };
    config.bundle = { createUpdaterArtifacts: true };
  }
  if (appStoreBuildNumber) {
    config.bundle = { ...config.bundle, macOS: { bundleVersion: appStoreBuildNumber } };
  }
  if (windowsCertificateThumbprint) {
    config.bundle = {
      ...config.bundle,
      windows: { certificateThumbprint: windowsCertificateThumbprint, digestAlgorithm: "sha256", timestampUrl: "http://timestamp.digicert.com" },
    };
  }
  return config;
}
