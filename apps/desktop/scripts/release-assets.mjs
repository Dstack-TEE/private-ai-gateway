import { createHash } from "node:crypto";
import { readdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";

const [directory] = process.argv.slice(2);
if (!directory) throw new Error("Supply the release artifact directory");
const entries = await readdir(directory, { recursive: true, withFileTypes: true });
const files = entries.filter((entry) => entry.isFile()).map((entry) => path.join(entry.parentPath, entry.name));
const assets = files.filter((file) => {
  const name = path.basename(file);
  if (/^private-ai-(gateway|proxy)-cli/.test(name)) return /\.(tar\.gz|zip)$/.test(name);
  return name === "latest.json" || /\.(dmg|exe|deb|rpm|app\.tar\.gz)$/.test(name);
}).sort();
if (new Set(assets.map((file) => path.basename(file))).size !== assets.length) throw new Error("Duplicate release asset names");
const sums = [];
for (const file of assets) {
  sums.push(`${createHash("sha256").update(await readFile(file)).digest("hex")}  ${path.basename(file)}`);
}
const checksum = path.join(directory, "SHA256SUMS");
await writeFile(checksum, sums.join("\n") + "\n");
// NUL-separated paths for bash mapfile, preserving spaces in official bundle names.
process.stdout.write([...assets, checksum].join("\0") + "\0");
