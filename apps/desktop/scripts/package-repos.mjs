#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFile, link, mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { gzipSync } from "node:zlib";
import semver from "semver";

import { archiveKeyring, linuxPackages } from "./package-linux.mjs";
import { artifactName, releaseChecksumAssets } from "./release-artifacts.mjs";
import { publishedRelease } from "./release-channel.mjs";
import { downloadSignedRelease } from "./release-download.mjs";

const retention = { stable: 5, beta: 10 };
const architectures = {
  x64: { deb: "amd64", rpm: "x86_64" },
  arm64: { deb: "arm64", rpm: "aarch64" },
};
const name = linuxPackages.desktop.name;
const pool = `pool/main/${name[0]}/${name}`;
const tauri = JSON.parse(await readFile(new URL("../src-tauri/tauri.conf.json", import.meta.url)));

export function selectReleases(entries) {
  const releases = [];
  for (const entry of entries) {
    const { tag_name: tag, draft, prerelease, assets } = entry;
    if (draft || !tag.startsWith("desktop-v")) continue;
    if (!assets.some(({ name }) => name === releaseChecksumAssets[1])) continue;
    // A withdrawn stable release is marked prerelease to remove it from
    // stable distribution. Only actual beta versions enter the beta suite.
    if (prerelease && semver.valid(tag.slice("desktop-v".length)) && !semver.prerelease(tag.slice("desktop-v".length))) continue;
    releases.push(publishedRelease(tag, prerelease));
  }
  releases.sort((a, b) => semver.rcompare(a.version, b.version));
  return Object.fromEntries(Object.entries(retention).map(([channel, count]) => [
    channel, releases.filter((release) => release.channel === channel).slice(0, count),
  ]));
}

function packagesFor(version, arch, format) {
  return Object.keys(linuxPackages).map((kind) => artifactName({
    version, platform: "linux", arch, suffix: `.${format}`, cli: kind === "cli",
  }));
}

function command(program, args, cwd) {
  return execFileSync(program, args, { cwd, maxBuffer: 8 * 1024 * 1024, stdio: ["ignore", "pipe", "inherit"] });
}

async function writeApt(root, channel, releases, downloads, sign) {
  const apt = path.join(root, "apt");
  const distribution = `dists/${channel}`;
  await mkdir(path.join(apt, pool), { recursive: true });
  const entries = [];
  for (const [arch, native] of Object.entries(architectures)) {
    const binary = path.join(distribution, "main", `binary-${native.deb}`);
    await mkdir(path.join(apt, binary), { recursive: true });
    const stanzas = [];
    for (const { version, tag } of releases) {
      for (const file of packagesFor(version, arch, "deb")) {
        const relative = path.join(pool, file);
        await link(path.join(downloads, tag, file), path.join(apt, relative));
        // Resolve each package against the repository root. A shared pool
        // contains both suites, so scanning the entire pool would mix them.
        stanzas.push(command("apt-ftparchive", ["packages", relative], apt));
      }
    }
    const packages = Buffer.concat(stanzas);
    for (const [suffix, bytes] of [["", packages], [".gz", gzipSync(packages)]]) {
      const entry = path.join(apt, binary, `Packages${suffix}`);
      await writeFile(entry, bytes);
      entries.push([entry, bytes]);
    }
  }
  const options = {
    Origin: tauri.bundle.publisher,
    Label: tauri.productName,
    Suite: channel,
    Codename: channel,
    Architectures: Object.values(architectures).map(({ deb }) => deb).join(" "),
    Components: "main",
    "Acquire-By-Hash": "yes",
  };
  const release = path.join(apt, distribution, "Release");
  await writeFile(release, command("apt-ftparchive", [
    ...Object.entries(options).flatMap(([key, value]) => ["-o", `APT::FTPArchive::Release::${key}=${value}`]),
    "release", distribution,
  ], apt));
  for (const [entry, bytes] of entries) {
    for (const algorithm of ["sha256", "sha512"]) {
      const directory = path.join(path.dirname(entry), "by-hash", algorithm.toUpperCase());
      await mkdir(directory, { recursive: true });
      const hashed = path.join(directory, createHash(algorithm).update(bytes).digest("hex"));
      await writeFile(hashed, bytes);
    }
  }
  sign(["--clearsign", "--output", path.join(path.dirname(release), "InRelease"), release]);
  sign(["--armor", "--detach-sign", "--output", `${release}.gpg`, release]);
}

async function writeRpm(root, channel, releases, downloads, sign) {
  for (const [arch, native] of Object.entries(architectures)) {
    const repo = path.join(root, "rpm", channel, native.rpm);
    await mkdir(repo, { recursive: true });
    for (const { version, tag } of releases) {
      for (const file of packagesFor(version, arch, "rpm")) {
        await link(path.join(downloads, tag, file), path.join(repo, file));
      }
    }
    command("createrepo_c", [repo]);
    const metadata = path.join(repo, "repodata", "repomd.xml");
    sign(["--armor", "--detach-sign", "--output", `${metadata}.asc`, metadata]);
    await copyFile(path.join(root, "key.asc"), `${metadata}.key`);
  }
}

export async function buildRepos({ root, baseUrl, downloads, releases, publicKey, keyring, sign }) {
  const url = new URL(baseUrl);
  if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
    throw new Error("PKG_REPO_URL must be an HTTP(S) base URL");
  }
  const base = url.href.replace(/\/$/, "");
  await mkdir(root);
  await copyFile(publicKey, path.join(root, "key.asc"));
  await copyFile(keyring, path.join(root, path.basename(`${archiveKeyring}.gpg`)));
  for (const [channel, versions] of Object.entries(releases)) {
    const id = channel === "stable" ? name : `${name}-${channel}`;
    await writeApt(root, channel, versions, downloads, sign);
    await writeRpm(root, channel, versions, downloads, sign);
    await writeFile(path.join(root, "apt", `${id}.sources`),
      `Types: deb\nURIs: ${base}/apt\nSuites: ${channel}\nComponents: main\nSigned-By: ${archiveKeyring}.gpg\n`);
    // DNF and libzypp both expand $basearch.
    await writeFile(path.join(root, "rpm", `${id}.repo`), [
      `[${id}]`, `name=${tauri.productName} ${channel}`, `baseurl=${base}/rpm/${channel}/$basearch`,
      "enabled=1", "type=rpm-md", "gpgcheck=1", "repo_gpgcheck=1", "pkg_gpgcheck=1", `gpgkey=${base}/key.asc`, "",
    ].join("\n"));
  }
}

async function main() {
  const [workDir, repository, baseUrl, tag] = process.argv.slice(2);
  if (!workDir || !/^[\w.-]+\/[\w.-]+$/.test(repository ?? "") || !baseUrl) {
    throw new Error("Usage: package-repos.mjs WORK_DIR OWNER/REPOSITORY BASE_URL [RELEASE_TAG]");
  }
  const { PAP_LINUX_SIGNING_PUBLIC_KEY: publicKey, PAP_LINUX_SIGNING_KEYRING: keyring,
    PAP_LINUX_SIGNING_FINGERPRINT: fingerprint, RUNNER_TEMP: runnerTemp } = process.env;
  if (!publicKey || !keyring || !fingerprint || !runnerTemp) throw new Error("Prepare Linux signing first");
  const entries = command("gh", [
    "api", "--paginate", `repos/${repository}/releases?per_page=100`, "--jq",
    `.[] | select(.tag_name | startswith("desktop-v")) | {tag_name, draft, prerelease, assets: [.assets[] | select(.name == "${releaseChecksumAssets[1]}") | {name}]}`,
  ]).toString().trim().split("\n").filter(Boolean).map((line) => JSON.parse(line));
  const releases = selectReleases(entries);
  if (!Object.values(releases).some((channel) => channel.length)) {
    throw new Error("No signed desktop releases are published yet");
  }
  if (tag && !entries.some((entry) => entry.tag_name === tag && !entry.draft
    && entry.assets.some(({ name }) => name === releaseChecksumAssets[1]))) {
    throw new Error(`The published release ${tag} has no signed checksums`);
  }
  const downloads = path.resolve(workDir, "downloads");
  for (const release of Object.values(releases).flat()) {
    const files = Object.keys(architectures).flatMap((arch) =>
      ["deb", "rpm"].flatMap((format) => packagesFor(release.version, arch, format)));
    await downloadSignedRelease(repository, release.tag, path.join(downloads, release.tag), files, keyring);
  }
  await buildRepos({
    root: path.resolve(workDir, "repo"), baseUrl, downloads, releases, publicKey, keyring,
    sign: (args) => command("gpg", [
      "--batch", "--no-tty", "--homedir", path.join(runnerTemp, "pap-linux-signing", "gnupg"),
      "--pinentry-mode", "loopback", "--passphrase", "", "--local-user", `${fingerprint}!`, ...args,
    ]),
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) await main();
