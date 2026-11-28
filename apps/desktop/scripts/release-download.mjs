import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { mkdir, readFile } from "node:fs/promises";
import path from "node:path";

import { releaseChecksumAssets } from "./release-artifacts.mjs";

export function downloadReleaseFiles(repository, tag, directory, names) {
  execFileSync("gh", [
    "release", "download", tag, "--repo", repository, "--dir", directory,
    ...names.flatMap((name) => ["--pattern", name]),
  ], { stdio: "inherit" });
}

export async function verifyReleaseFiles(directory, names) {
  const sums = new Map();
  for (const line of (await readFile(path.join(directory, releaseChecksumAssets[0]), "utf8")).trimEnd().split("\n")) {
    const match = /^([0-9a-f]{64}) [ *]([\w.+-]+)$/.exec(line);
    if (!match || sums.has(match[2])) throw new Error("Invalid SHA256SUMS");
    sums.set(match[2], match[1]);
  }
  const verified = {};
  for (const name of names) {
    if (!sums.has(name)) throw new Error(`SHA256SUMS is missing ${name}`);
    const hash = createHash("sha256");
    for await (const chunk of createReadStream(path.join(directory, name))) hash.update(chunk);
    if (hash.digest("hex") !== sums.get(name)) throw new Error(`SHA256 checksum mismatch for ${name}`);
    verified[name] = sums.get(name);
  }
  return verified;
}

export async function downloadSignedRelease(repository, tag, directory, names, keyring) {
  await mkdir(directory, { recursive: true });
  downloadReleaseFiles(repository, tag, directory, [...releaseChecksumAssets, ...names]);
  execFileSync("gpgv", ["--keyring", path.resolve(keyring), releaseChecksumAssets[1], releaseChecksumAssets[0]], {
    cwd: directory, stdio: "inherit",
  });
  return verifyReleaseFiles(directory, names);
}
