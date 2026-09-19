import { createHash } from "node:crypto";
import { readdir, readFile, rename, writeFile } from "node:fs/promises";
import path from "node:path";
import { releaseChannel } from "./release-channel.mjs";
import { artifactName, desktopBuilds, desktopPackages, linuxNativePackageSuffixes } from "./release-artifacts.mjs";

const [directory] = process.argv.slice(2);
if (!directory) throw new Error("Supply the release artifact directory");
const entries = await readdir(directory, { recursive: true, withFileTypes: true });
const files = entries.filter((entry) => entry.isFile()).map((entry) => path.join(entry.parentPath, entry.name));
const manifest = JSON.parse(await readFile(path.join(directory, "latest.json"), "utf8"));
releaseChannel(manifest.version, manifest.channel);
const assetNames = releaseAssetNames(manifest.version);
const assets = files
  .map((file) => ({ file, name: releaseAssetName(file, assetNames, manifest.version) }))
  .filter((asset) => asset.name)
  .sort((a, b) => a.name.localeCompare(b.name));
const normalized = assets.map(({ file, name }) => path.join(path.dirname(file), name.replaceAll(" ", ".")));
if (new Set(normalized.map((file) => path.basename(file))).size !== assets.length) throw new Error("Duplicate release asset names");
const sums = [];
for (const [index, file] of normalized.entries()) {
  // Use the same portable names for upload paths and checksum entries.
  if (file !== assets[index].file) await rename(assets[index].file, file);
  sums.push(`${createHash("sha256").update(await readFile(file)).digest("hex")}  ${path.basename(file)}`);
}
const checksum = path.join(directory, "SHA256SUMS");
await writeFile(checksum, sums.join("\n") + "\n");
// NUL-separated paths for bash mapfile, preserving spaces in official bundle names.
process.stdout.write([...normalized, checksum].join("\0") + "\0");

function releaseAssetNames(version) {
  const names = new Set(["latest.json"]);
  for (const specification of desktopPackages) {
    const canonical = artifactName({ version, ...specification });
    names.add(canonical);
  }
  for (const { platform, arch } of desktopBuilds) {
    if (platform === "macos") {
      const diskImage = artifactName({ version, platform, arch, suffix: ".dmg" });
      names.add(diskImage);
    }
    if (platform === "linux") {
      names.add(artifactName({ version, platform, arch, suffix: ".pkg.tar.zst" }));
    }
    const archive = artifactName({
      version,
      platform,
      arch,
      suffix: platform === "windows" ? ".zip" : ".tar.gz",
      cli: true,
    });
    names.add(archive);
    if (platform === "linux") {
      for (const suffix of linuxNativePackageSuffixes) {
        const nativePackage = artifactName({ version, platform, arch, suffix, cli: true });
        names.add(nativePackage);
      }
    }
  }
  return names;
}

function releaseAssetName(file, names, version) {
  const name = path.basename(file);
  if (names.has(name)) return name;
  if (/^private-ai-proxy(?:-cli)?[-_].*\.(?:dmg|exe|deb|rpm|zip|tar\.gz|app\.tar\.gz|pkg\.tar\.zst)$/.test(name)) {
    throw new Error(`Release asset ${name} does not match version ${version} or a supported platform`);
  }
  return undefined;
}
