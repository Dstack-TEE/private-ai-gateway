import { readdir, readFile, stat, unlink, writeFile } from "node:fs/promises";
import path from "node:path";
import { releaseChannel } from "./release-channel.mjs";
import { artifactName, desktopPackages, releaseAssetNames, releaseChecksumAssets } from "./release-artifacts.mjs";

// Writes Tauri's static update manifest for the signed packages the package
// jobs staged under their public names in <directory>, checks the staged asset
// set, then removes the Tauri signature files now embedded in latest.json.
const [directory, version, repository, channel] = process.argv.slice(2);
const release = releaseChannel(version, channel);
if (!directory || !/^[\w.-]+\/[\w.-]+$/.test(repository ?? "")) {
  throw new Error("Usage: create-update-manifest.mjs <artifact directory> <version> <owner/repo> <beta|stable>");
}
const platforms = {};
const updaterSignatures = [];
for (const specification of desktopPackages) {
  const filename = artifactName({ version, ...specification });
  if (!(await stat(path.join(directory, filename)).catch(() => undefined))?.isFile()) throw new Error(`Missing update package ${filename}`);
  const signatureName = `${filename}.sig`;
  const signature = (await readFile(path.join(directory, signatureName), "utf8")).trim();
  if (!signature) throw new Error(`Missing signature for ${filename}`);
  platforms[specification.target] = { signature, url: `https://github.com/${repository}/releases/download/${release.tag}/${filename}` };
  updaterSignatures.push(signatureName);
}
await writeFile(path.join(directory, "latest.json"), `${JSON.stringify({ version, channel, pub_date: new Date().toISOString(), platforms }, null, 2)}\n`);
// The release job writes the checksum list and its signature after latest.json.
const expected = [...releaseAssetNames(version).filter((name) => !releaseChecksumAssets.includes(name)), ...updaterSignatures].sort();
const actual = (await readdir(directory)).sort();
if (JSON.stringify(actual) !== JSON.stringify(expected)) {
  const unexpected = actual.filter((name) => !expected.includes(name));
  const missing = expected.filter((name) => !actual.includes(name));
  throw new Error(`Release assets differ from the expected set. Unexpected: ${unexpected.join(", ") || "none"}. Missing: ${missing.join(", ") || "none"}`);
}
for (const signature of updaterSignatures) await unlink(path.join(directory, signature));
