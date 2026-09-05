import { execFileSync } from "node:child_process";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import path from "node:path";
import { publishedRelease, shouldAdvance } from "./release-channel.mjs";

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
for (const platform of ["darwin-aarch64", "windows-x86_64", "linux-x86_64"]) {
  const entry = manifest.platforms?.[platform];
  if (typeof entry?.signature !== "string" || !entry.signature.trim() || typeof entry.url !== "string" || !entry.url.startsWith(prefix)) throw new Error(`Invalid update entry: ${platform}`);
  await request(entry.url, { method: "HEAD" });
}

let feed;
try {
  feed = JSON.parse(gh("release", "view", release.feedTag, "--json", "assets"));
} catch (error) {
  if (!/release not found|HTTP 404/i.test(String(error.stderr ?? ""))) throw error;
}
let current;
if (feed?.assets.some((asset) => asset.name === "latest.json")) {
  current = await (await request(`https://github.com/${repo}/releases/download/${release.feedTag}/latest.json`)).json();
  if (current.channel !== release.channel) throw new Error("Existing feed belongs to another channel");
}
if (!shouldAdvance(release.version, current?.version, release.channel)) {
  console.log(`Keeping newer or equal ${release.channel} feed`);
} else {
  if (!feed) gh("release", "create", release.feedTag, "--target", process.env.GITHUB_SHA, "--title", `Desktop ${release.channel} update feed`, "--notes", "Signed desktop update manifest.", "--prerelease", "--latest=false");
  const directory = await mkdtemp(path.resolve(".update-feed-"));
  try {
    const file = path.join(directory, "latest.json");
    await writeFile(file, `${JSON.stringify(manifest, null, 2)}\n`);
    gh("release", "upload", release.feedTag, file, "--clobber");
    console.log(`Advanced ${release.channel} to ${release.version}`);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}
