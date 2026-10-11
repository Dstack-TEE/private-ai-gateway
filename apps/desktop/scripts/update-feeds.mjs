import { desktopTargets } from "./release-artifacts.mjs";

// The assets a release writes to one channel feed. Tauri's static manifest has
// one version for all its packages: never merge packages from different
// releases beneath that version.
export function updateFeeds(manifest, selectedTargets, feed) {
  const feeds = new Map();
  // Current clients read latest.json. Betas predating per-platform feeds
  // (before 0.1.2-beta.44) read it too but accept only beta manifests, so a
  // stable release in the beta feed shows them a channel mismatch until the
  // next beta is published.
  if (desktopTargets.every((target) => selectedTargets.includes(target))) {
    feeds.set("latest.json", manifest);
  }
  // Clients up to 0.1.7-beta.4 read per-platform files (0.1.7-beta.2 to
  // beta.4 from both feeds, earlier ones from their own channel's feed) and
  // reject a manifest from the other channel. Removal in 0.3.
  if (feed === manifest.channel) {
    for (const target of selectedTargets) {
      const name = `latest-${target.replace(/-(deb|rpm)$/, "")}.json`;
      if (!feeds.has(name)) feeds.set(name, { ...manifest, platforms: {} });
      feeds.get(name).platforms[target] = manifest.platforms[target];
    }
  }
  return feeds;
}
