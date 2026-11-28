#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { copyFile, mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";

import { linuxPackages, markerDirectory, releaseVersionParts } from "./package-linux.mjs";
import { artifactName, releaseChecksumAssets } from "./release-artifacts.mjs";
import { publishedRelease } from "./release-channel.mjs";

const [output, repository, tag] = process.argv.slice(2);
if (!output || !/^[\w.-]+\/[\w.-]+$/.test(repository ?? "")) {
  throw new Error("Usage: package-aur.mjs OUTPUT OWNER/REPOSITORY RELEASE_TAG");
}
const { tag_name: publishedTag, draft, prerelease, assets } = JSON.parse(execFileSync("gh", [
  "api", `repos/${repository}/releases/tags/${publishedRelease(tag, false).tag}`,
], { encoding: "utf8" }));
if (draft || publishedTag !== tag) throw new Error("AUR requires a public stable release");
const { version } = publishedRelease(tag, prerelease);
if (prerelease) throw new Error("AUR requires a public stable release");
const urls = new Map(assets.map(({ name, browser_download_url: url }) => [name, url]));
const assetUrl = (name) => {
  const url = urls.get(name);
  if (url !== `https://github.com/${repository}/releases/download/${tag}/${name}`) {
    throw new Error(`Missing or unexpected release asset URL for ${name}`);
  }
  return url;
};

const publicKey = new URL("../packaging/linux-signing-key.asc", import.meta.url);
await mkdir(output);
const keyring = path.resolve(output, "release-keyring.gpg");
const gpg = (args) => execFileSync("gpg", ["--batch", ...args], { stdio: ["ignore", "pipe", "inherit"] });
gpg(["--output", keyring, "--dearmor", publicKey.pathname]);
const primary = gpg(["--with-colons", "--show-keys", publicKey.pathname]).toString().split("\n")
  .find((line) => line.startsWith("fpr:"))?.split(":")[9];
if (!/^[A-F0-9]{40}$/.test(primary ?? "")) throw new Error("Missing primary signing fingerprint");
for (const file of releaseChecksumAssets) assetUrl(file);
execFileSync("gh", [
  "release", "download", tag, "--repo", repository, "--dir", output,
  ...releaseChecksumAssets.flatMap((name) => ["--pattern", name]),
], { stdio: "inherit" });
const [checksums, signature] = releaseChecksumAssets.map((file) => path.join(output, file));
execFileSync("gpgv", ["--keyring", keyring, signature, checksums], { stdio: "inherit" });
const hashes = new Map((await readFile(checksums, "utf8")).trimEnd().split("\n").map((line) => {
  const match = /^([a-f0-9]{64}) [ *]([\w.+-]+)$/.exec(line);
  if (!match) throw new Error("Invalid SHA256SUMS");
  return [match[2], match[1]];
}));

const app = JSON.parse(await readFile(new URL("../src-tauri/tauri.conf.json", import.meta.url)));
const { license } = JSON.parse(await readFile(new URL("../package.json", import.meta.url)));
const quote = (value) => "'" + value.replaceAll("'", "'\\''") + "'";
const array = (values) => "(" + values.map(quote).join(" ") + ")";
const architectures = { x86_64: "x64", aarch64: "arm64" };
const pkgver = releaseVersionParts(version).arch;
const preremove = await readFile(new URL("../src-tauri/installer/linux-preremove.sh", import.meta.url), "utf8");
for (const [kind, definition] of Object.entries(linuxPackages)) {
  const pkgname = `${definition.name}-bin`;
  const directory = path.join(output, pkgname);
  await mkdir(directory);
  const files = Object.entries(architectures).map(([native, arch]) => [native, artifactName({
    version, platform: "linux", arch, suffix: ".pkg.tar.zst", cli: kind === "cli",
  })]);
  const lines = [
    `pkgname=${pkgname}`,
    `pkgver=${pkgver}`,
    "pkgrel=1",
    `pkgdesc=${quote(definition.description.split("\n")[0])}`,
    `arch=${array(Object.keys(architectures))}`,
    `url=${quote(app.bundle.homepage)}`,
    `license=${array([license])}`,
    `depends=${array(definition.depends.archlinux)}`,
    `provides=(${quote(definition.name)}"=$pkgver"${definition.provides ? ` ${quote(definition.other)}"=$pkgver"` : ""})`,
    `conflicts=${array([definition.name, definition.other])}`,
    "options=('!strip' '!debug')",
    "install=preremove.install",
    `validpgpkeys=${array([primary])}`,
    `noextract=${array(files.map(([, file]) => file))}`,
  ];
  for (const [architecture, file] of files) {
    const sig = `${file}.sig`;
    if (!hashes.has(file) || !hashes.has(sig)) throw new Error(`Missing signed checksums for ${file}`);
    lines.push(
      `source_${architecture}=${array([assetUrl(file), assetUrl(sig)])}`,
      `sha256sums_${architecture}=${array([hashes.get(file), "SKIP"])}`,
    );
  }
  lines.push("", "package() {", "  local archive", '  case "$CARCH" in');
  for (const [arch, file] of files) lines.push(`    ${arch}) archive=${quote(file)} ;;`);
  lines.push(
    "  esac",
    '  bsdtar -xpf "$srcdir/$archive" -C "$pkgdir" usr/',
    `  printf '%s\\n' aur > "$pkgdir"${quote(`${markerDirectory}/package-manager`)}`,
    "}", "",
  );
  await writeFile(path.join(directory, "PKGBUILD"), lines.join("\n"));
  await writeFile(path.join(directory, "preremove.install"), `pre_remove() (\n${preremove}\n)\n`);
  await copyFile(new URL("../packaging/aur/LICENSE", import.meta.url), path.join(directory, "LICENSE"));
}
