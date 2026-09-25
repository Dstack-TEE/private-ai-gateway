#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

// This helper runs in the OIDC publish job, which intentionally installs no
// dependencies, so it only uses Node built-ins.
const scriptPath = fileURLToPath(import.meta.url);
const npmPackageName = "private-ai-proxy";

const defaultRegistry = "https://registry.npmjs.org";

export async function tarballIdentity(tarball) {
  const manifest = JSON.parse(execFileSync("tar", ["-xOzf", tarball, "package/package.json"], {
    encoding: "utf8",
  }));
  if (manifest.name !== npmPackageName || typeof manifest.version !== "string") {
    throw new Error(`${tarball} is not a ${npmPackageName} tarball`);
  }
  const digest = createHash("sha512").update(await readFile(tarball)).digest("base64");
  return {
    name: manifest.name,
    version: manifest.version,
    integrity: `sha512-${digest}`,
  };
}

// Reports whether a version already exists, without authentication. Returns
// "missing", "published" or "unavailable"; throws when the registry has the
// version with different contents, which a rerun must never paper over.
export async function registryVersionState(identity, { registry = defaultRegistry, fetchImpl = fetch } = {}) {
  const url = `${registry}/${identity.name}/${identity.version}`;
  let response;
  try {
    response = await fetchImpl(url, { headers: { accept: "application/json" } });
  } catch (error) {
    return { state: "unavailable", reason: `${url}: ${error.message}` };
  }
  if (response.status === 404) return { state: "missing" };
  if (!response.ok) return { state: "unavailable", reason: `${url}: HTTP ${response.status}` };
  let body;
  try {
    body = await response.json();
  } catch {
    return { state: "unavailable", reason: `${url}: invalid JSON` };
  }
  if (body?.version !== identity.version) {
    return { state: "unavailable", reason: `registry returned version ${body?.version}` };
  }
  if (body.dist?.integrity !== identity.integrity) {
    throw new Error(`${identity.name}@${identity.version} exists in the registry with different contents`);
  }
  return { state: "published" };
}

async function main() {
  const [command, tarball, ...rest] = process.argv.slice(2);
  if (command !== "state" || !tarball || rest.length > 0) {
    throw new Error("Usage: npm-registry.mjs state <tarball>");
  }
  const result = await registryVersionState(await tarballIdentity(path.resolve(tarball)));
  if (result.state === "unavailable") throw new Error(`Registry lookup failed: ${result.reason}`);
  console.log(result.state);
}

if (process.argv[1] && path.resolve(process.argv[1]) === scriptPath) {
  await main();
}
