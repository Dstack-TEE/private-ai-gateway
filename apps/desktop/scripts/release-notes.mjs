import { readFile, readdir } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { releaseChannel } from "./release-channel.mjs";

const platforms = [
  { platform: "macos", arch: "arm64", title: "macOS Apple Silicon" },
  { platform: "macos", arch: "x64", title: "macOS Intel" },
  { platform: "windows", arch: "x64", title: "Windows x64" },
  { platform: "windows", arch: "arm64", title: "Windows ARM64" },
  { platform: "linux", arch: "x64", title: "Linux x64" },
  { platform: "linux", arch: "arm64", title: "Linux ARM64" },
];
const installerExtensions = [".dmg", ".exe", ".deb", ".rpm"];
const portableExtensions = [".zip", ".tar.gz"];

export async function buildReleaseNotes(directory, repository, commit, summary = "") {
  if (!directory || !/^[\w.-]+\/[\w.-]+$/.test(repository ?? "")) {
    throw new Error("Supply artifact directory and repository");
  }
  if (!/^[0-9a-f]{40}$/.test(commit ?? "")) throw new Error("Supply the 40-character build commit");

  const manifest = JSON.parse(await readFile(path.join(directory, "latest.json"), "utf8"));
  const release = releaseChannel(manifest.version, manifest.channel);
  const files = new Set(
    (await readdir(directory, { recursive: true, withFileTypes: true }))
      .filter((entry) => entry.isFile())
      .map((entry) => entry.name),
  );
  const link = (name) => `https://github.com/${repository}/releases/download/${release.tag}/${name}`;
  const releaseSummary = summary.trim();
  if (!release.prerelease && !releaseSummary) throw new Error("Stable release notes require a summary");
  const lines = [
    release.prerelease
      ? "> Beta channel build for testing. It may change before the stable release."
      : "Production release on the stable update channel.",
    "",
    "## What's changed",
    "",
    releaseSummary || "Testing build for the linked commit.",
  ];
  lines.push("", "## Downloads", "", "| Platform | Installer |", "| --- | --- |");

  for (const specification of platforms) {
    const prefix = `private-ai-proxy-${release.version}-${specification.platform}-${specification.arch}`;
    const installers = matchingFiles(files, prefix, installerExtensions);
    if (installers.length > 0) {
      lines.push(`| ${specification.title} | ${installers.map((name) => assetLink(name, link)).join(" · ")} |`);
    }
  }

  if ([...files].some((name) => name.endsWith(".dmg"))) {
    lines.push("", "Download the DMG for a manual macOS installation. The `.app.tar.gz` files and `latest.json` are updater assets.");
  }
  lines.push(
    "",
    "<details>",
    "<summary>Standalone CLI downloads</summary>",
    "",
    "| Platform | Archive |",
    "| --- | --- |",
  );
  for (const specification of platforms) {
    const prefix = `private-ai-proxy-cli-${release.version}-${specification.platform}-${specification.arch}`;
    const extensions = specification.platform === "linux"
      ? [".tar.gz", ".deb", ".rpm"]
      : portableExtensions;
    const archives = matchingFiles(files, prefix, extensions);
    if (archives.length > 0) {
      lines.push(`| ${specification.title} | ${archives.map((name) => assetLink(name, link)).join(" · ")} |`);
    }
  }
  lines.push(
    "",
    "</details>",
    "",
    "## Integrity and updates",
    "",
    `- [SHA-256 checksums](${link("SHA256SUMS")})`,
    "- Automatic updates verify Tauri signatures before installation.",
  );
  if ([...files].some((name) => name.endsWith(".dmg"))) {
    lines.push("- macOS desktop packages are Developer ID signed and notarized.");
  }
  lines.push(`- Build commit: [\`${commit.slice(0, 7)}\`](https://github.com/${repository}/commit/${commit})`, "");
  return lines.join("\n");
}

function matchingFiles(files, prefix, extensions) {
  return extensions
    .map((extension) => `${prefix}${extension}`)
    .filter((name) => files.has(name));
}

function assetLink(name, link) {
  const extension = name.endsWith(".tar.gz") ? "TAR.GZ" : path.extname(name).slice(1).toUpperCase();
  return `[${extension}](${link(name)})`;
}

const scriptPath = fileURLToPath(import.meta.url);
if (process.argv[1] && path.resolve(process.argv[1]) === scriptPath) {
  const [directory, repository, commit] = process.argv.slice(2);
  process.stdout.write(await buildReleaseNotes(directory, repository, commit, process.env.RELEASE_SUMMARY));
}
