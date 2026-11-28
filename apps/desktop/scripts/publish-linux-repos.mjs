#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { readdir } from "node:fs/promises";
import path from "node:path";

const [root] = process.argv.slice(2);
const { PKG_REPO_R2_ENDPOINT: endpoint, PKG_REPO_R2_BUCKET: bucket } = process.env;
if (!root || !endpoint || !bucket) {
  throw new Error("Set PKG_REPO_R2_ENDPOINT and PKG_REPO_R2_BUCKET and supply the repository directory");
}
const immutable = (key) => /^(apt\/pool\/|rpm\/.+\.rpm$)|\/by-hash\/|\/repodata\/(?!repomd\.xml)/.test(key);
const validKey = (key) => typeof key === "string" && /^[\w.-]+(?:\/[\w.-]+)*$/.test(key)
  && !key.split("/").some((part) => part === "." || part === "..");
const listFile = ".repo-generations.json";
const s3 = `s3://${bucket}`;
const aws = (args, options) => execFileSync("aws", ["--endpoint-url", endpoint, ...args], options);
const files = (await readdir(root, { recursive: true, withFileTypes: true }))
  .filter((entry) => entry.isFile())
  .map((entry) => path.relative(root, path.join(entry.parentPath, entry.name))).sort();
if (!files.every(validKey)) throw new Error("Unexpected path in repository tree");

// Read the last two successful inventories. Only remove immutable objects
// listed there, and only after the new metadata is reachable.
const inventory = JSON.parse(aws(["s3api", "list-objects-v2", "--bucket", bucket, "--prefix", listFile]));
const previous = inventory.Contents?.some(({ Key }) => Key === listFile)
  ? JSON.parse(aws(["s3", "cp", "--only-show-errors", `${s3}/${listFile}`, "-"]))
  : { current: [], previous: [] };
if (![previous.current, previous.previous].every((keys) => Array.isArray(keys)
  && keys.every((key) => validKey(key) && immutable(key)))) {
  throw new Error("Invalid repository generation inventory");
}

function upload(verb, patterns, cache) {
  aws(["s3", verb, root, s3, ...(verb === "cp" ? ["--recursive"] : ["--size-only"]),
    "--only-show-errors", "--cache-control", cache, "--exclude", "*",
    ...patterns.flatMap((pattern) => ["--include", pattern])], { stdio: "inherit" });
}

const longCache = "public, max-age=31536000, immutable";
upload("sync", ["apt/pool/*", "rpm/*/*.rpm"], longCache);
upload("sync", ["*/by-hash/*", "*/repodata/*-*"], longCache);
upload("cp", ["key.asc", "*-archive-keyring.gpg", "*/Packages*", "*/repomd.xml.key"], "no-cache");
upload("cp", ["*/Release", "*/Release.gpg", "*/repomd.xml", "*/repomd.xml.asc"], "no-cache");
upload("cp", ["*/InRelease", "apt/*.sources", "rpm/*.repo"], "no-cache");

const current = files.filter(immutable);
const keep = new Set([...current, ...previous.current]);
for (const key of previous.previous) {
  if (!keep.has(key)) aws(["s3", "rm", "--only-show-errors", `${s3}/${key}`], { stdio: "inherit" });
}
aws(["s3", "cp", "--only-show-errors", "--cache-control", "no-cache", "-", `${s3}/${listFile}`], {
  input: JSON.stringify({ current, previous: previous.current }),
});
