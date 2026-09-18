import { createHash } from "node:crypto";
import { readdir, readFile, rename, writeFile } from "node:fs/promises";
import path from "node:path";
import { releaseChannel } from "./release-channel.mjs";
import { artifactName, desktopBuilds, desktopPackages } from "./release-artifacts.mjs";
import { releaseVersionParts } from "./package-cli.mjs";

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
  const names = new Map();
  names.set("latest.json", "latest.json");
  for (const specification of desktopPackages) {
    const canonical = artifactName({ version, ...specification });
    names.set(canonical, canonical);
  }
  for (const { platform, arch } of desktopBuilds) {
    if (platform === "macos") {
      const diskImage = artifactName({ version, platform, arch, suffix: ".dmg" });
      names.set(diskImage, diskImage);
    }
    const archive = artifactName({
      version,
      platform,
      arch,
      suffix: platform === "windows" ? ".zip" : ".tar.gz",
      cli: true,
    });
    names.set(archive, archive);
  }

  const { deb } = releaseVersionParts(version);
  for (const [packageArch, arch] of [["amd64", "x64"], ["arm64", "arm64"]]) {
    const canonical = artifactName({ version, platform: "linux", arch, suffix: ".deb", cli: true });
    names.set(canonical, canonical);
    names.set(`private-ai-proxy-cli_${deb}_${packageArch}.deb`, canonical);
  }
  for (const [packageArch, arch] of [["x86_64", "x64"], ["aarch64", "arm64"]]) {
    const canonical = artifactName({ version, platform: "linux", arch, suffix: ".rpm", cli: true });
    names.set(canonical, canonical);
    names.set(`private-ai-proxy-cli-${version}.${packageArch}.rpm`, canonical);
  }
  return names;
}

function releaseAssetName(file, names, version) {
  const name = path.basename(file);
  const normalized = names.get(name);
  if (normalized) return normalized;
  if (/^private-ai-proxy(?:-cli)?[-_].*\.(?:dmg|exe|deb|rpm|zip|tar\.gz|app\.tar\.gz)$/.test(name)) {
    throw new Error(`Release asset ${name} does not match version ${version} or a supported platform`);
  }
  return undefined;
}
