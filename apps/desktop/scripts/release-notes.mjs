import { readFile, readdir } from "node:fs/promises";
import path from "node:path";
import { releaseChannel } from "./release-channel.mjs";

const [directory, repository] = process.argv.slice(2);
if (!directory || !/^[\w.-]+\/[\w.-]+$/.test(repository ?? "")) throw new Error("Supply artifact directory and repository");
const manifest = JSON.parse(await readFile(path.join(directory, "latest.json"), "utf8"));
const release = releaseChannel(manifest.version, manifest.channel);
const files = (await readdir(directory, { recursive: true, withFileTypes: true })).filter(entry => entry.isFile()).map(entry => entry.name);
const link = name => `https://github.com/${repository}/releases/download/${release.tag}/${name}`;
console.log(`## Download\n\n${release.prerelease ? "Beta release. " : ""}Choose your operating system and processor.\n\n| Platform | Installer |\n| --- | --- |`);
for (const platform of ["macos", "windows", "linux"]) {
  for (const arch of ["arm64", "x64"]) {
    const prefix = `private-ai-proxy-${release.version}-${platform}-${arch}`;
    const installers = files.filter(name => name.startsWith(prefix + ".") && /\.(dmg|exe|deb|rpm)$/.test(name));
    if (!installers.length) continue;
    const title = platform === "macos" ? `macOS ${arch === "arm64" ? "Apple Silicon" : "Intel"}` : `${platform === "windows" ? "Windows" : "Linux"} ${arch === "arm64" ? "ARM64" : "x64"}`;
    console.log(`| ${title} | ${installers.map(name => `[${path.extname(name).slice(1).toUpperCase()}](${link(name)})`).join(" · ")} |`);
  }
}
console.log("\nFor manual macOS installation, download the DMG. The .app.tar.gz files are used by automatic updates; you do not need both.\n\n<details>\n<summary>Standalone CLI downloads</summary>\n");
for (const name of files.filter(name => name.startsWith(`private-ai-proxy-cli-${release.version}-`) && /\.(zip|tar\.gz)$/.test(name)).sort()) console.log(`- [${name}](${link(name)})`);
console.log(`\n</details>\n\n[SHA256 checksums](${link("SHA256SUMS")})`);
