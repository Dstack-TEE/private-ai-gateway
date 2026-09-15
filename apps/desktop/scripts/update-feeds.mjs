import { desktopPackages } from "./release-artifacts.mjs";

// Tauri's static manifest has one version for all its packages. Never merge
// packages from different releases beneath that version.
export function updateFeeds(manifest, selectedTargets) {
  const feeds = new Map();
  for (const target of selectedTargets) {
    const name = `latest-${target.replace(/-(deb|rpm)$/, "")}.json`;
    if (!feeds.has(name)) feeds.set(name, { ...manifest, platforms: {} });
    feeds.get(name).platforms[target] = manifest.platforms[target];
  }
  const allTargets = desktopPackages.flatMap((entry) => entry.targets);
  if (allTargets.every((target) => selectedTargets.includes(target))) {
    // Clients predating per-platform feeds only receive complete releases.
    feeds.set("latest.json", manifest);
  }
  return feeds;
}
