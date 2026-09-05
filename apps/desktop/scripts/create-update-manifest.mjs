import { readdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";

const [directory, version, repository] = process.argv.slice(2);
if (!directory || !/^\d+\.\d+\.\d+$/.test(version ?? "") || !/^[\w.-]+\/[\w.-]+$/.test(repository ?? "")) {
  throw new Error("Usage: create-update-manifest.mjs <artifact directory> <version> <owner/repo>");
}
const entries = await readdir(directory, { recursive: true, withFileTypes: true });
const files = entries.filter((entry) => entry.isFile()).map((entry) => path.join(entry.parentPath, entry.name));
const tag = `desktop-v${version}`;
const platforms = {};
const suffixes = { "darwin-aarch64": ".app.tar.gz", "windows-x86_64": ".exe", "linux-x86_64": ".AppImage" };
for (const [target, suffix] of Object.entries(suffixes)) {
  const candidates = files.filter((file) => file.endsWith(suffix));
  if (candidates.length !== 1) throw new Error(`Expected one ${target} update package; found ${candidates.length}`);
  const file = candidates[0];
  const signature = (await readFile(`${file}.sig`, "utf8")).trim();
  if (!signature) throw new Error(`Missing signature for ${target}`);
  platforms[target] = {
    signature,
    url: `https://github.com/${repository}/releases/download/${tag}/${encodeURIComponent(path.basename(file))}`,
  };
}
await writeFile(path.join(directory, "latest.json"), `${JSON.stringify({ version, pub_date: new Date().toISOString(), platforms }, null, 2)}\n`);
