import { desktopTargets } from "./release-artifacts.mjs";

// The assets a release writes to one channel feed. Tauri's static manifest has
// one version for all its packages: never merge packages from different
// releases beneath that version.
export function updateFeeds(manifest, selectedTargets, feed) {
  const feeds = new Map();
  if (desktopTargets.every((target) => selectedTargets.includes(target))) {
    feeds.set("latest.json", manifest);
  }
  // Clients released before 0.2.0 read per-platform files from both feeds
  // and reject a manifest from the other channel. Removal in 0.3.
  if (feed === manifest.channel) {
    for (const target of selectedTargets) {
      const name = `latest-${target.replace(/-(deb|rpm)$/, "")}.json`;
      if (!feeds.has(name)) feeds.set(name, { ...manifest, platforms: {} });
      feeds.get(name).platforms[target] = manifest.platforms[target];
    }
  }
  return feeds;
}
