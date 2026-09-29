import { readFile, readdir } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { artifactName, desktopPackages } from "./release-artifacts.mjs";
import { releaseChannel } from "./release-channel.mjs";

// release-please writes the changelog as the draft release's notes; the
// release job appends the download links for the assets it attached. The
// markers let a rerun replace the section instead of adding a second one.
const sectionStart = "<!-- desktop-downloads -->";
const sectionEnd = "<!-- /desktop-downloads -->";
const macAppStore = "https://apps.apple.com/app/private-ai-proxy/id6814051406";

const platformTitles = {
  "macos-arm64": "macOS Apple Silicon",
  "macos-x64": "macOS Intel",
  "windows-x64": "Windows x64",
  "windows-arm64": "Windows ARM64",
  "linux-x64": "Linux x64",
  "linux-arm64": "Linux ARM64",
};
// One row per platform, in build-matrix order.
const platforms = [...new Map(desktopPackages.map(({ platform, arch }) => [`${platform}-${arch}`, { platform, arch }])).entries()]
  .map(([id, build]) => {
    const title = platformTitles[id];
    if (!title) throw new Error(`Missing release-note title for ${id}`);
    return { ...build, title };
  });
const linuxPackages = [".deb", ".rpm", ".pkg.tar.zst"];
const installerSuffixes = { macos: [".dmg"], windows: [".exe"], linux: linuxPackages };
const archiveSuffixes = { macos: [".tar.gz"], windows: [".zip"], linux: [".tar.gz", ...linuxPackages] };

/** The Downloads and integrity section for the release in `directory`, whose latest.json names the version. */
export async function downloadSection(directory, repository, commit) {
  if (!directory || !/^[\w.-]+\/[\w.-]+$/.test(repository ?? "")) {
    throw new Error("Supply artifact directory and repository");
  }
  if (!/^[0-9a-f]{40}$/.test(commit ?? "")) throw new Error("Supply the 40-character build commit");

  const manifest = JSON.parse(await readFile(path.join(directory, "latest.json"), "utf8"));
  const release = releaseChannel(manifest.version, manifest.channel);
  const files = new Set(
    (await readdir(directory, { withFileTypes: true }))
      .filter((entry) => entry.isFile())
      .map((entry) => entry.name),
  );
  const link = (name) => `https://github.com/${repository}/releases/download/${release.tag}/${name}`;
  const row = (title, names) => `| ${title} | ${names.map((name) => assetLink(name, link)).join(" · ")} |`;
  const present = (names) => names.filter((name) => files.has(name));

  const lines = [sectionStart, "", "## Downloads", "", "| Platform | Installer |", "| --- | --- |"];
  for (const { platform, arch, title } of platforms) {
    const installers = present(installerSuffixes[platform].map((suffix) => artifactName({ version: release.version, platform, arch, suffix })));
    if (installers.length > 0) lines.push(row(title, installers));
  }
  const dmg = [...files].some((name) => name.endsWith(".dmg"));
  if (dmg) lines.push("", "Download the DMG for a manual macOS installation. The `.app.tar.gz` files and `latest.json` are updater assets.");
  // Only stable versions are uploaded to the App Store.
  if (!release.prerelease) lines.push("", `Also on the [Mac App Store](${macAppStore}).`);

  lines.push("", "<details>", "<summary>Standalone CLI downloads</summary>", "", "| Platform | Archive |", "| --- | --- |");
  for (const { platform, arch, title } of platforms) {
    const archives = present(archiveSuffixes[platform].map((suffix) => artifactName({ version: release.version, platform, arch, suffix, cli: true })));
    if (archives.length > 0) lines.push(row(title, archives));
  }
  lines.push(
    "",
    "</details>",
    "",
    "## Integrity and updates",
    "",
    `- [SHA-256 checksums](${link("SHA256SUMS")}): run \`sha256sum --ignore-missing -c SHA256SUMS\` in the download directory.`,
    `- Build provenance and SBOMs: \`gh attestation verify <file> --repo ${repository}\`.`,
    "- Automatic updates verify Tauri signatures before installation.",
  );
  if (dmg) lines.push("- macOS desktop packages are Developer ID signed and notarized.");
  lines.push(`- Build commit: [\`${commit.slice(0, 7)}\`](https://github.com/${repository}/commit/${commit})`, "", sectionEnd);
  return lines.join("\n");
}

/** `notes` followed by `section`, replacing a section an earlier run added. */
export function withDownloads(notes, section) {
  const start = notes.indexOf(sectionStart);
  const end = notes.indexOf(sectionEnd, start);
  const changelog = start === -1 || end === -1 ? notes : notes.slice(0, start) + notes.slice(end + sectionEnd.length);
  return `${changelog.trimEnd()}\n\n${section}\n`;
}

function assetLink(name, link) {
  const format = name.endsWith(".pkg.tar.zst") ? "ARCH"
    : name.endsWith(".tar.gz") ? "TAR.GZ"
    : path.extname(name).slice(1).toUpperCase();
  return `[${format}](${link(name)})`;
}

async function readStdin() {
  const chunks = [];
  for await (const chunk of process.stdin) chunks.push(chunk);
  return Buffer.concat(chunks).toString("utf8");
}

// Reads the current notes on stdin and prints them with the download section:
// node release-notes.mjs <artifact-directory> <owner/repository> <commit> < notes.md
const scriptPath = fileURLToPath(import.meta.url);
if (process.argv[1] && path.resolve(process.argv[1]) === scriptPath) {
  const [directory, repository, commit] = process.argv.slice(2);
  const section = await downloadSection(directory, repository, commit);
  process.stdout.write(withDownloads(await readStdin(), section));
}
