import { execFileSync } from "node:child_process";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import path from "node:path";
import { feedTag, publishedRelease, shouldAdvance } from "./release-channel.mjs";
import { manifestTargets } from "./release-artifacts.mjs";
import { updateFeeds } from "./update-feeds.mjs";

const repo = process.env.GH_REPO;
const tag = process.env.TAG;
if (!/^[\w.-]+\/[\w.-]+$/.test(repo ?? "")) throw new Error("Invalid repository");
const gh = (...args) => execFileSync("gh", [...args, "--repo", repo], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
const metadata = JSON.parse(gh("release", "view", tag, "--json", "tagName,isDraft,isPrerelease"));
if (metadata.isDraft) throw new Error("Draft releases cannot advance an update channel");
const release = publishedRelease(metadata.tagName, metadata.isPrerelease);
const prefix = `https://github.com/${repo}/releases/download/${tag}/`;
const request = async (url, options = {}) => {
  const response = await fetch(url, { ...options, signal: AbortSignal.timeout(30000) });
  if (!response.ok) throw new Error(`Update asset unavailable (${response.status}): ${url}`);
  return response;
};
const manifest = await (await request(`${prefix}latest.json`)).json();
if (manifest.version !== release.version || manifest.channel !== release.channel) throw new Error("Manifest and release channel do not match");
const selectedTargets = manifestTargets(manifest);
for (const platform of selectedTargets) {
  const entry = manifest.platforms?.[platform];
  if (typeof entry?.signature !== "string" || !entry.signature.trim() || typeof entry.url !== "string" || !entry.url.startsWith(prefix)) throw new Error(`Invalid update entry: ${platform}`);
  await request(entry.url, { method: "HEAD" });
}

// Stable releases also advance the beta feed so beta users receive them.
for (const channel of release.channel === "stable" ? ["stable", "beta"] : ["beta"]) {
  await advance(channel);
}

async function advance(channel) {
  const feedRelease = feedTag(channel);
  let feed;
  try {
    feed = JSON.parse(gh("release", "view", feedRelease, "--json", "assets"));
  } catch (error) {
    if (!/release not found|HTTP 404/i.test(String(error.stderr ?? ""))) throw error;
  }
  const pending = [];
  for (const [name, candidate] of updateFeeds(manifest, selectedTargets, channel)) {
    let current;
    if (feed?.assets.some((asset) => asset.name === name)) {
      current = await (await request(`https://github.com/${repo}/releases/download/${feedRelease}/${name}`)).json();
    }
    if (shouldAdvance(release.version, current?.version, channel)) {
      pending.push([name, candidate]);
    }
  }
  if (!pending.length) {
    console.log(`Keeping newer or equal ${channel} feed`);
    return;
  }
  const title = `Private AI Proxy ${channel} update feed`;
  const notes = "Signed Private AI Proxy update manifests. This release is maintained by automation.";
  const releaseFlags = [`--prerelease=${channel === "beta"}`, "--latest=false"];
  if (feed) gh("release", "edit", feedRelease, "--title", title, "--notes", notes, ...releaseFlags);
  else gh("release", "create", feedRelease, "--target", process.env.GITHUB_SHA, "--title", title, "--notes", notes, ...releaseFlags);
  const directory = await mkdtemp(path.resolve(".update-feed-"));
  try {
    const files = [];
    for (const [name, candidate] of pending) {
      const file = path.join(directory, name);
      await writeFile(file, `${JSON.stringify(candidate, null, 2)}\n`);
      files.push(file);
    }
    gh("release", "upload", feedRelease, ...files, "--clobber");
    console.log(`Advanced ${channel} feed to ${release.version}`);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}
