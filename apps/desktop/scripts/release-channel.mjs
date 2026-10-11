import { readFileSync } from "node:fs";
import semver from "semver";

const channels = ["beta", "stable"];

export function releaseChannel(version, channel = "beta") {
  if (!channels.includes(channel)) throw new Error("Release channel must be beta or stable");
  if (typeof version !== "string" || semver.valid(version) !== version || semver.parse(version).build.length) {
    throw new Error("Release version must be a canonical semantic version without build metadata");
  }
  const prerelease = semver.prerelease(version);
  const beta = prerelease?.length === 2 && prerelease[0] === "beta" && Number.isSafeInteger(prerelease[1]) && prerelease[1] > 0;
  if (channel === "beta" ? !beta : prerelease !== null) {
    throw new Error(channel === "beta" ? "Beta versions must use x.y.z-beta.n (n >= 1)" : "Stable versions must use x.y.z");
  }
  return { channel, version, tag: `desktop-v${version}`, feedTag: feedTag(channel), prerelease: channel === "beta" };
}

export function feedTag(channel) {
  return `desktop-updates-${channel}`;
}

// The version release-please commits to every manifest (release-please-config.json).
export function appVersion() {
  return JSON.parse(readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8")).version;
}

// Betas are x.y.z-beta.n; every other release ships on the stable channel.
export function versionRelease(version) {
  return releaseChannel(version, semver.valid(version) && semver.prerelease(version) ? "beta" : "stable");
}

// Mirrors belongs_to_feed in core/src/updates.rs: stable releases are also
// published to the beta feed, as electron-builder's
// generateUpdatesFilesForAllChannels publishes them to every lower channel.
function assertInFeed(version, feed) {
  releaseChannel(version, feed === "beta" && semver.valid(version) && !semver.prerelease(version) ? "stable" : feed);
}

export function shouldAdvance(candidate, current, feed) {
  assertInFeed(candidate, feed);
  if (!current) return true;
  assertInFeed(current, feed);
  return semver.gt(candidate, current);
}

export function publishedRelease(tag, prerelease) {
  if (typeof tag !== "string" || !tag.startsWith("desktop-v") || typeof prerelease !== "boolean") throw new Error("Invalid desktop release metadata");
  return releaseChannel(tag.slice("desktop-v".length), prerelease ? "beta" : "stable");
}
