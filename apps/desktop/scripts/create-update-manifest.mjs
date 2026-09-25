import { readdir, readFile, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { releaseChannel } from "./release-channel.mjs";
import { artifactName, desktopPackages, releaseAssetNames } from "./release-artifacts.mjs";

// Writes Tauri's static update manifest for the signed packages the package
// jobs staged under their public names in <directory>, then checks that the
// directory holds exactly the release's assets: it is published as is.
const [directory, version, repository, channel] = process.argv.slice(2);
const release = releaseChannel(version, channel);
if (!directory || !/^[\w.-]+\/[\w.-]+$/.test(repository ?? "")) {
  throw new Error("Usage: create-update-manifest.mjs <artifact directory> <version> <owner/repo> <beta|stable>");
}
const platforms = {};
for (const specification of desktopPackages) {
  const filename = artifactName({ version, ...specification });
  if (!(await stat(path.join(directory, filename)).catch(() => undefined))?.isFile()) throw new Error(`Missing update package ${filename}`);
  const signature = (await readFile(path.join(directory, `${filename}.sig`), "utf8")).trim();
  if (!signature) throw new Error(`Missing signature for ${filename}`);
  platforms[specification.target] = { signature, url: `https://github.com/${repository}/releases/download/${release.tag}/${filename}` };
}
await writeFile(path.join(directory, "latest.json"), `${JSON.stringify({ version, channel, pub_date: new Date().toISOString(), platforms }, null, 2)}\n`);
const expected = [...releaseAssetNames(version), ...desktopPackages.map((entry) => `${artifactName({ version, ...entry })}.sig`)].sort();
const actual = (await readdir(directory)).sort();
if (JSON.stringify(actual) !== JSON.stringify(expected)) {
  const unexpected = actual.filter((name) => !expected.includes(name));
  const missing = expected.filter((name) => !actual.includes(name));
  throw new Error(`Release assets differ from the expected set. Unexpected: ${unexpected.join(", ") || "none"}. Missing: ${missing.join(", ") || "none"}`);
}
