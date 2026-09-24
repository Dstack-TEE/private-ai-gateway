#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";

// This helper runs in the OIDC publish job, which intentionally installs no
// dependencies, so it only uses Node built-ins.
const scriptPath = fileURLToPath(import.meta.url);
// The wrapper and its scoped per-platform packages.
const packageNamePattern = /^(?:private-ai-proxy|@phala\/private-ai-proxy-(?:darwin|linux|win32)-(?:arm64|x64))$/;

export const defaultRegistry = "https://registry.npmjs.org";
// The abbreviated ("corgi") packument is the document npm install resolves
// versions from, so a version is only installable once it appears here.
const installAccept = "application/vnd.npm.install-v1+json";

export async function tarballIdentity(tarball) {
  const manifest = JSON.parse(execFileSync("tar", ["-xOzf", tarball, "package/package.json"], {
    encoding: "utf8",
  }));
  if (!packageNamePattern.test(manifest.name) || typeof manifest.version !== "string") {
    throw new Error(`${tarball} is not a private-ai-proxy tarball`);
  }
  const digest = createHash("sha512").update(await readFile(tarball)).digest("base64");
  return {
    name: manifest.name,
    version: manifest.version,
    integrity: `sha512-${digest}`,
  };
}

// Scoped names keep their `@` but escape the `/`, as npm-package-arg does.
function packageUrl(registry, name) {
  return `${registry}/${name.replace("/", "%2f")}`;
}

function spec(identity) {
  return `${identity.name}@${identity.version}`;
}

async function fetchJson(fetchImpl, url, headers = {}) {
  let response;
  try {
    response = await fetchImpl(url, { headers: { accept: "application/json", ...headers } });
  } catch (error) {
    return { state: "unavailable", reason: `${url}: ${error.message}` };
  }
  if (response.status === 404) return { state: "missing" };
  if (!response.ok) return { state: "unavailable", reason: `${url}: HTTP ${response.status}` };
  try {
    return { state: "ok", body: await response.json() };
  } catch {
    return { state: "unavailable", reason: `${url}: invalid JSON` };
  }
}

function checkIntegrity(identity, integrity, source) {
  if (integrity !== identity.integrity) {
    throw new Error(
      `${spec(identity)} exists in the ${source} with different contents`,
    );
  }
}

// Reports whether a version already exists, without authentication. Returns
// "missing", "published" or "unavailable"; throws when the registry has the
// version with different contents, which a rerun must never paper over.
export async function registryVersionState(identity, { registry = defaultRegistry, fetchImpl = fetch } = {}) {
  const result = await fetchJson(fetchImpl, `${packageUrl(registry, identity.name)}/${identity.version}`);
  if (result.state !== "ok") return result;
  if (result.body?.version !== identity.version) {
    return { state: "unavailable", reason: `registry returned version ${result.body?.version}` };
  }
  checkIntegrity(identity, result.body.dist?.integrity, "version document");
  return { state: "published" };
}

// A version is visible once both its version document and the install
// packument report it with the expected integrity.
export async function registryVisibility(identity, options = {}) {
  const { registry = defaultRegistry, fetchImpl = fetch } = options;
  const version = await registryVersionState(identity, { registry, fetchImpl });
  if (version.state !== "published") return version;
  const packument = await fetchJson(fetchImpl, packageUrl(registry, identity.name), { accept: installAccept });
  if (packument.state !== "ok") return packument;
  const entry = packument.body?.versions?.[identity.version];
  if (!entry) return { state: "missing", reason: "not yet in the install packument" };
  checkIntegrity(identity, entry.dist?.integrity, "install packument");
  return { state: "published" };
}

export async function waitForRegistry(identities, {
  registry = defaultRegistry,
  fetchImpl = fetch,
  timeoutMs = 15 * 60_000,
  intervalMs = 10_000,
  now = Date.now,
  sleep = delay,
  log = console.log,
} = {}) {
  const deadline = now() + timeoutMs;
  let pending = [...identities];
  for (;;) {
    const remaining = [];
    for (const identity of pending) {
      const result = await registryVisibility(identity, { registry, fetchImpl });
      if (result.state === "published") {
        log(`${spec(identity)} is visible on ${registry}`);
      } else {
        remaining.push({ identity, reason: result.reason ?? result.state });
      }
    }
    if (remaining.length === 0) return;
    if (now() >= deadline) {
      const details = remaining.map(({ identity, reason }) => `${spec(identity)} (${reason})`);
      throw new Error(`Timed out waiting for ${registry} to serve ${details.join(", ")}`);
    }
    log(`Waiting for ${remaining.map(({ identity }) => spec(identity)).join(", ")}`);
    pending = remaining.map(({ identity }) => identity);
    await sleep(Math.min(intervalMs, Math.max(0, deadline - now())));
  }
}

function parseArguments(arguments_) {
  const [command, ...rest] = arguments_;
  const usage = "Usage: npm-registry.mjs <state|wait> [--timeout-seconds <n>] [--interval-seconds <n>] <tarball>...";
  if (!["state", "wait"].includes(command)) throw new Error(usage);
  const options = { command, tarballs: [], timeoutMs: 15 * 60_000, intervalMs: 10_000 };
  for (let index = 0; index < rest.length; index += 1) {
    const argument = rest[index];
    if (argument === "--timeout-seconds" || argument === "--interval-seconds") {
      const seconds = Number(rest[index + 1]);
      if (command !== "wait" || !Number.isInteger(seconds) || seconds < 0) throw new Error(usage);
      options[argument === "--timeout-seconds" ? "timeoutMs" : "intervalMs"] = seconds * 1000;
      index += 1;
    } else if (argument.startsWith("--")) {
      throw new Error(`Unknown argument ${argument}`);
    } else {
      options.tarballs.push(path.resolve(argument));
    }
  }
  if (options.tarballs.length === 0 || (command === "state" && options.tarballs.length !== 1)) {
    throw new Error(usage);
  }
  return options;
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  const identities = await Promise.all(options.tarballs.map(tarballIdentity));
  if (options.command === "state") {
    const result = await registryVersionState(identities[0]);
    if (result.state === "unavailable") throw new Error(`Registry lookup failed: ${result.reason}`);
    console.log(result.state);
    return;
  }
  await waitForRegistry(identities, options);
}

if (process.argv[1] && path.resolve(process.argv[1]) === scriptPath) {
  await main();
}
