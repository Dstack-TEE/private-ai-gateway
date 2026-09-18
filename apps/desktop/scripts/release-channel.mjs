import semver from "semver";

export function releaseChannel(version, channel = "beta") {
  if (!['beta', 'stable'].includes(channel)) throw new Error("Release channel must be beta or stable");
  if (typeof version !== "string" || semver.valid(version) !== version || semver.parse(version).build.length) {
    throw new Error("Release version must be a canonical semantic version without build metadata");
  }
  const prerelease = semver.prerelease(version);
  const beta = prerelease?.length === 2 && prerelease[0] === "beta" && Number.isSafeInteger(prerelease[1]) && prerelease[1] > 0;
  if (channel === "beta" ? !beta : prerelease !== null) {
    throw new Error(channel === "beta" ? "Beta versions must use x.y.z-beta.n (n >= 1)" : "Stable versions must use x.y.z");
  }
  return { channel, version, tag: `desktop-v${version}`, feedTag: `desktop-updates-${channel}`, prerelease: channel === "beta" };
}

export function shouldAdvance(candidate, current, channel) {
  releaseChannel(candidate, channel);
  if (!current) return true;
  releaseChannel(current, channel);
  return semver.gt(candidate, current);
}

export function publishedRelease(tag, prerelease) {
  if (typeof tag !== "string" || !tag.startsWith("desktop-v") || typeof prerelease !== "boolean") throw new Error("Invalid desktop release metadata");
  return releaseChannel(tag.slice("desktop-v".length), prerelease ? "beta" : "stable");
}
