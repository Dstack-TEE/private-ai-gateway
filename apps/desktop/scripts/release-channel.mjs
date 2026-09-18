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
  return { channel, version, tag: `desktop-v${version}`, feedTag: `desktop-updates-${channel}`, prerelease: channel === "beta" };
}

export function releaseTitle(version, channel = "beta") {
  return `Private AI Proxy v${releaseChannel(version, channel).version}`;
}

export function validateReleaseRequest({ version = "", channel = "beta", platforms = "all", ref = "", publish = false, packageOnly = false, summary = "" }) {
  if (!version) {
    if (publish) throw new Error("Publishing requires a release version");
    return undefined;
  }
  if (packageOnly) throw new Error("package_only cannot be used for a release");
  const release = releaseChannel(version, channel);
  const notes = summary.trim();
  if (notes.length > 8000) throw new Error("Release summary must be 8000 characters or fewer");
  if (/^#{1,6}\s/m.test(notes)) throw new Error("Release summary must not contain Markdown headings");
  if (release.channel === "stable") {
    if (ref !== "refs/heads/main") throw new Error("Stable releases must be built from main");
    if (platforms.trim() !== "all") throw new Error("Stable releases must include every supported platform");
    if (!notes) throw new Error("Stable releases require a release summary");
  }
  return release;
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
