#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { dump } from "js-yaml";

import { aliases } from "./package-linux.mjs";
import { binaries } from "./package-cli.mjs";
import { artifactName, desktopPackages, releaseChecksumAssets } from "./release-artifacts.mjs";
import { publishedRelease } from "./release-channel.mjs";

const [output, repository, tag, legacyOption] = process.argv.slice(2);
if (!output || !/^[\w.-]+\/[\w.-]+$/.test(repository ?? "") || (legacyOption && legacyOption !== "--legacy")) {
  throw new Error("Usage: package-winget.mjs OUTPUT OWNER/REPOSITORY RELEASE_TAG [--legacy]");
}
const { version } = publishedRelease(tag, false);
const release = JSON.parse(execFileSync("gh", [
  "api", `repos/${repository}/releases/tags/${tag}`,
], { encoding: "utf8" }));
if (release.draft || release.prerelease || release.tag_name !== tag) {
  throw new Error("WinGet requires a public stable release");
}

const app = JSON.parse(await readFile(new URL("../src-tauri/tauri.conf.json", import.meta.url)));
const { license } = JSON.parse(await readFile(new URL("../package.json", import.meta.url)));
const { publisher, homepage, shortDescription, longDescription } = app.bundle;
const compact = (value) => value.replace(/[^a-zA-Z0-9]/g, "");
const identifier = `${compact(publisher)}.${compact(app.productName)}`;
const web = `https://github.com/${repository}`;
const manifestVersion = "1.28.0";
const locale = "en-US";
const installers = desktopPackages.filter(({ platform }) => platform === "windows");
const pairs = installers.flatMap((entry) => [
  { ...entry, cli: false }, { ...entry, suffix: ".zip", cli: true },
].map((variant) => ({ ...variant, name: artifactName({ version, ...variant }) })));
const [checksumFile] = releaseChecksumAssets;
const legacy = !release.assets.some(({ name }) => name === releaseChecksumAssets[1]);
if (legacy && legacyOption !== "--legacy") throw new Error("Missing signed release checksums");
const scratch = await mkdtemp(path.join(os.tmpdir(), "pap-winget-"));
try {
  const checksums = legacy ? [checksumFile] : releaseChecksumAssets;
  execFileSync("gh", [
    "release", "download", tag, "--repo", repository, "--dir", scratch,
    ...[...checksums, ...pairs.map(({ name }) => name)].flatMap((name) => ["--pattern", name]),
  ], { stdio: "inherit" });
  if (!legacy) {
    const key = new URL("../packaging/linux-signing-key.asc", import.meta.url).pathname;
    const keyring = path.join(scratch, "release-keyring.gpg");
    execFileSync("gpg", ["--batch", "--dearmor", "--output", keyring, key], { stdio: "inherit" });
    execFileSync("gpgv", ["--keyring", keyring, ...checksums.toReversed()], { cwd: scratch, stdio: "inherit" });
  }
  const sums = new Map((await readFile(path.join(scratch, checksumFile), "utf8")).trimEnd().split("\n").map((line) => {
    const match = /^([0-9a-f]{64}) [ *]([\w.+-]+)$/.exec(line);
    if (!match) throw new Error("Invalid SHA256SUMS");
    return [match[2], match[1]];
  }));
  const entries = [];
  for (const { arch, cli, name } of pairs) {
    const asset = release.assets.find((asset) => asset.name === name);
    if (asset?.browser_download_url !== `${web}/releases/download/${tag}/${name}`) {
      throw new Error(`Missing or unexpected release asset URL for ${name}`);
    }
    const file = path.join(scratch, name);
    const hash = createHash("sha256");
    for await (const chunk of createReadStream(file)) hash.update(chunk);
    const digest = hash.digest("hex");
    if (sums.get(name) !== digest || (legacy && asset.digest !== `sha256:${digest}`)) {
      throw new Error(`SHA256 checksum mismatch for ${name}`);
    }
    const installer = {
      Architecture: arch, InstallerUrl: asset.browser_download_url, InstallerSha256: digest.toUpperCase(),
      // The CLI and the desktop installer's CLI registration hook load VCRUNTIME140.dll.
      Dependencies: { PackageDependencies: [{ PackageIdentifier: `Microsoft.VCRedist.2015+.${arch}` }] },
    };
    if (cli) {
      const directory = path.basename(name, ".zip");
      const contents = new Set(execFileSync("unzip", ["-Z1", file], { encoding: "utf8" }).trimEnd().split("\n"));
      for (const binary of binaries) {
        if (!contents.has(`${directory}/${binary}.exe`)) throw new Error(`Missing ${binary}.exe in ${name}`);
      }
      installer.NestedInstallerFiles = [{
        RelativeFilePath: `${directory}/${binaries[0]}.exe`,
        PortableCommandAlias: aliases[0],
      }];
    }
    entries.push({ cli, installer });
  }

  for (const cli of [false, true]) {
    const id = cli ? `${identifier}.CLI` : identifier;
    const manifestDir = path.join(output, "manifests", id[0].toLowerCase(), ...id.split("."), version);
    await mkdir(manifestDir, { recursive: true });
    const write = (kind, suffix, contents) => writeFile(path.join(manifestDir, `${id}${suffix}.yaml`),
      `# yaml-language-server: $schema=https://aka.ms/winget-manifest.${kind}.${manifestVersion}.schema.json\n`
        + dump({ PackageIdentifier: id, PackageVersion: version, ...contents, ManifestType: kind, ManifestVersion: manifestVersion }, { lineWidth: -1 }));
    await write("version", "", { DefaultLocale: locale });
    await write("defaultLocale", `.locale.${locale}`, {
      PackageLocale: locale, Publisher: publisher, PublisherUrl: homepage,
      PublisherSupportUrl: `${web}/issues`, PackageName: cli ? `${app.productName} CLI` : app.productName,
      PackageUrl: web, License: license, LicenseUrl: `${web}/blob/${tag}/LICENSE`,
      ShortDescription: cli ? `${app.productName} command-line client` : shortDescription,
      Description: longDescription, ReleaseNotesUrl: release.html_url,
    });
    await write("installer", ".installer", {
      InstallerType: cli ? "zip" : "nullsoft",
      ...(cli ? { NestedInstallerType: "portable", Commands: [aliases[0]] } : {
        Scope: app.bundle.windows.nsis.installMode === "currentUser" ? "user" : "machine",
        ProductCode: app.productName,
        AppsAndFeaturesEntries: [{ DisplayName: app.productName, Publisher: publisher, ProductCode: app.productName }],
      }),
      ReleaseDate: release.published_at.slice(0, 10),
      Installers: entries.filter((entry) => entry.cli === cli).map(({ installer }) => installer),
    });
  }
} finally {
  await rm(scratch, { recursive: true, force: true });
}
