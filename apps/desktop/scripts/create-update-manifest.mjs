import { readdir, readFile, rename, writeFile } from "node:fs/promises";
import path from "node:path";
import { releaseChannel } from "./release-channel.mjs";
import { artifactName, desktopPackages } from "./release-artifacts.mjs";

const [directory, version, repository, channel = "beta", selectedPlatforms = "all"] = process.argv.slice(2);
const release = releaseChannel(version, channel);
if (!directory || !/^[\w.-]+\/[\w.-]+$/.test(repository ?? "")) {
  throw new Error("Usage: create-update-manifest.mjs <artifact directory> <version> <owner/repo> [beta|stable] [all|platform,...]");
}
const entries = await readdir(directory, { recursive: true, withFileTypes: true });
const files = entries.filter((entry) => entry.isFile()).map((entry) => path.join(entry.parentPath, entry.name));
const tag = release.tag;
const platforms = {};
const requested = selectedPlatforms === "all" ? null : new Set(selectedPlatforms.split(",").map((value) => value.trim()).filter(Boolean));
const supported = new Set(desktopPackages.map(({ platform, arch }) => `${platform}-${arch}`));
if (requested && (!requested.size || [...requested].some((platform) => !supported.has(platform)))) {
  throw new Error(`Platforms must be all or a subset of ${[...supported].join(", ")}`);
}
for (const specification of desktopPackages) {
  const packageId = `${specification.platform}-${specification.arch}`;
  if (requested && !requested.has(packageId)) continue;
  const { suffix, targets } = specification;
  const filename = artifactName({ version, ...specification });
  // download-artifact keeps matrix job names as directories. Use that provenance
  // to distinguish native x64/ARM64 filenames before normalizing signed assets.
  const candidates = files.filter((file) => {
    if (path.basename(file) === filename) return true;
    const container = path.relative(directory, file).split(path.sep)[0];
    const prefix = `private-ai-proxy-${channel}-${version}-`;
    const identity = container.startsWith(prefix) ? container.slice(prefix.length) : "";
    return /^[a-f0-9]{40}-/.test(identity) && identity.slice(41) === packageId
      && file.endsWith(suffix) && !path.basename(file).startsWith("private-ai-proxy-cli");
  });
  if (candidates.length !== 1) throw new Error(`Expected one ${suffix} update package; found ${candidates.length}`);
  const file = candidates[0];
  const signature = (await readFile(`${file}.sig`, "utf8")).trim();
  if (!signature) throw new Error(`Missing signature for ${suffix}`);
  // GitHub rewrites asset names containing spaces. Stage portable names first
  // so upload and manifest URLs refer to exactly the same signed bytes.
  const staged = path.join(path.dirname(file), filename);
  if (staged !== file) {
    await rename(file, staged);
    await rename(`${file}.sig`, `${staged}.sig`);
  }
  for (const target of targets) {
    platforms[target] = { signature, url: `https://github.com/${repository}/releases/download/${tag}/${filename}` };
  }
}
await writeFile(path.join(directory, "latest.json"), `${JSON.stringify({ version, channel, pub_date: new Date().toISOString(), platforms }, null, 2)}\n`);
