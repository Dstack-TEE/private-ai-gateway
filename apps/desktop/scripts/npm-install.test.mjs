// Installs the npm packages from a local registry with real package managers:
// npm always, pnpm and bun when they are on PATH. The registry is an
// in-process static server so the test needs no network or credentials.
import assert from "node:assert/strict";
import { execFile, execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, copyFile, mkdir, mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { promisify } from "node:util";

import { binaries } from "./package-cli.mjs";
import {
  buildPlatformPackage,
  buildWrapperPackage,
  npmArchitectures,
  npmPlatforms,
} from "./package-npm.mjs";

const hostPlatform = Object.keys(npmPlatforms).find((platform) => npmPlatforms[platform] === process.platform);
// The fake native executables are shell scripts.
const supportedHost = ["linux", "darwin"].includes(process.platform) && npmArchitectures.includes(process.arch);
const hostPackage = `@phala/private-ai-proxy-${process.platform}-${process.arch}`;
const npm = "npm";
// Installs must not block the event loop, which serves the local registry.
const execFileAsync = promisify(execFile);
const installTimeout = 120_000;

test("npm, pnpm and bun install the wrapper with only the host platform package", { skip: !supportedHost }, async (t) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "pap-npm-install-"));
  const registry = await startRegistry();
  try {
    const packages = path.join(root, "packages");
    // A legacy release, published the way private-ai-proxy <= 0.1.7-beta.4
    // was: the payloads are versions of the wrapper name behind npm aliases.
    for (const [version, tag] of [["0.9.0", "latest"], ["1.0.0-beta.1", "beta"]]) {
      for (const tarball of await buildLegacyRelease(root, version, packages)) {
        const payload = !tarball.endsWith(`private-ai-proxy-${version}.tgz`);
        // The owner checklist deprecates the payload versions.
        await registry.publish(tarball, payload ? { deprecated: "internal platform payload" } : { tag });
      }
    }

    // Before a release's platform packages are on the registry, the release
    // job installs the local tarballs together, as the workflow smoke test
    // does. The other platform packages do not exist yet.
    const beta = await buildRelease(root, "1.0.0-beta.2", packages);
    await installLocalTarballs(root, "unpublished-beta", beta, registry.url);

    const legacy = await npmInstall(root, "upgrade", ["private-ai-proxy"], registry.url);
    assert.equal(run(legacy.bin("pap"), "--version"), "private-ai-proxy 0.9.0");
    assert.ok((await readdir(legacy.modules)).includes(`private-ai-proxy-${process.platform}-${process.arch}`));

    for (const tarball of beta.tarballs) await registry.publish(tarball, { tag: "beta" });
    // A prerelease range resolves to a wrapper, never to a platform payload.
    const range = await npmInstall(root, "range", ["private-ai-proxy@^1.0.0-beta.1"], registry.url);
    assert.equal(run(range.bin("private-ai-proxy"), "--version"), "private-ai-proxy 1.0.0-beta.2");
    assert.deepEqual(await readdir(path.join(range.modules, "@phala")), [path.basename(hostPackage)]);

    // Now the other platform packages exist, but not at this version.
    const stable = await buildRelease(root, "1.0.0", packages);
    await installLocalTarballs(root, "unpublished-stable", stable, registry.url);
    for (const tarball of stable.tarballs) await registry.publish(tarball, { tag: "latest" });
    await npmInstall(root, "upgrade", ["private-ai-proxy@latest"], registry.url);
    for (const command of ["private-ai-proxy", "pap", "aci"]) {
      assert.equal(run(legacy.bin(command), "--version"), "private-ai-proxy 1.0.0");
    }
    assert.deepEqual(await readdir(path.join(legacy.modules, "@phala")), [path.basename(hostPackage)]);
    assert.ok(!(await readdir(legacy.modules)).some((name) => name.startsWith("private-ai-proxy-")));

    await t.test("pnpm", { skip: !available("pnpm") }, async () => {
      const project = await projectWith(root, "pnpm", "1.0.0");
      await execFileAsync("pnpm", [
        "install",
        `--registry=${registry.url}`,
        `--store-dir=${path.join(root, "pnpm-store")}`,
        `--cache-dir=${path.join(root, "pnpm-cache")}`,
      ], { cwd: project, env: packageManagerEnv(root), timeout: installTimeout });
      assert.equal(run(path.join(project, "node_modules/.bin/private-ai-proxy"), "--version"), "private-ai-proxy 1.0.0");
    });

    await t.test("bun", { skip: !available("bun") }, async () => {
      const project = await projectWith(root, "bun", "1.0.0");
      await execFileAsync("bun", [
        "install",
        `--registry=${registry.url}`,
        `--cache-dir=${path.join(root, "bun-cache")}`,
      ], { cwd: project, env: packageManagerEnv(root), timeout: installTimeout });
      assert.equal(run(path.join(project, "node_modules/.bin/private-ai-proxy"), "--version"), "private-ai-proxy 1.0.0");
    });
  } finally {
    await registry.close();
    await rm(root, { recursive: true, force: true });
  }
});

async function buildRelease(root, version, output) {
  const source = await fakeBinaries(root, version);
  const platforms = {};
  const tarballs = [];
  for (const platform of Object.keys(npmPlatforms)) {
    platforms[platform] = {};
    for (const arch of npmArchitectures) {
      const tarball = await buildPlatformPackage({ platform, arch, version, source, output });
      platforms[platform][arch] = tarball;
      tarballs.push(tarball);
    }
  }
  const wrapper = await buildWrapperPackage({ version, output });
  // Platform packages are published before the wrapper.
  return { version, platforms, wrapper, tarballs: [...tarballs, wrapper] };
}

async function buildLegacyRelease(root, version, output) {
  const source = await fakeBinaries(root, version);
  await mkdir(output, { recursive: true });
  const tarballs = [];
  const optionalDependencies = {};
  for (const npmPlatform of Object.values(npmPlatforms)) {
    for (const arch of npmArchitectures) {
      const target = `${npmPlatform}-${arch}`;
      optionalDependencies[`private-ai-proxy-${target}`] = `npm:private-ai-proxy@${version}-${target}`;
      const directory = path.join(root, "legacy", version, target);
      await mkdir(path.join(directory, "vendor"), { recursive: true });
      await copyFile(path.join(source, "private-ai-proxy"), path.join(directory, "vendor/private-ai-proxy"));
      await writeJson(path.join(directory, "package.json"), {
        name: "private-ai-proxy",
        version: `${version}-${target}`,
        os: [npmPlatform],
        cpu: [arch],
      });
      tarballs.push(pack(directory, output));
    }
  }
  const directory = path.join(root, "legacy", version, "wrapper");
  await mkdir(path.join(directory, "bin"), { recursive: true });
  // The essential behavior of the legacy launcher.
  await writeFile(path.join(directory, "bin/private-ai-proxy.cjs"), `#!/usr/bin/env node
const path = require("node:path");
const manifest = require.resolve(\`private-ai-proxy-\${process.platform}-\${process.arch}/package.json\`);
const result = require("node:child_process").spawnSync(path.join(path.dirname(manifest), "vendor/private-ai-proxy"), process.argv.slice(2), { stdio: "inherit" });
process.exitCode = result.status ?? 1;
`);
  await writeJson(path.join(directory, "package.json"), {
    name: "private-ai-proxy",
    version,
    bin: { "private-ai-proxy": "bin/private-ai-proxy.cjs", pap: "bin/private-ai-proxy.cjs", aci: "bin/private-ai-proxy.cjs" },
    optionalDependencies,
  });
  tarballs.push(pack(directory, output));
  return tarballs;
}

async function fakeBinaries(root, version) {
  const source = path.join(root, "source", version);
  await mkdir(source, { recursive: true });
  for (const binary of binaries) {
    for (const extension of ["", ".exe"]) {
      const file = path.join(source, `${binary}${extension}`);
      await writeFile(file, `#!/bin/sh\nprintf '${binary} ${version}\\n'\n`);
      await chmod(file, 0o755);
    }
  }
  return source;
}

async function npmInstall(root, name, specs, registry) {
  const prefix = path.join(root, "global", name);
  await execFileAsync(npm, [
    "install",
    "--global",
    "--prefix", prefix,
    "--registry", registry,
    "--no-audit",
    "--no-fund",
    ...specs,
  ], { env: packageManagerEnv(root), timeout: installTimeout });
  return {
    bin: (command) => path.join(prefix, "bin", command),
    modules: path.join(prefix, "lib/node_modules/private-ai-proxy/node_modules"),
  };
}

async function installLocalTarballs(root, name, release, registry) {
  const installed = await npmInstall(root, name, [
    `file:${release.platforms[hostPlatform][process.arch]}`,
    `file:${release.wrapper}`,
  ], registry);
  assert.equal(run(installed.bin("private-ai-proxy"), "--version"), `private-ai-proxy ${release.version}`);
}

async function projectWith(root, name, version) {
  const project = path.join(root, "projects", name);
  await mkdir(project, { recursive: true });
  await writeJson(path.join(project, "package.json"), {
    name: `pap-${name}-smoke`,
    private: true,
    dependencies: { "private-ai-proxy": version },
  });
  return project;
}

// Isolates every package manager from the user's configuration and from the
// npm_config_* variables `npm run` passes to this test.
function packageManagerEnv(root) {
  const env = Object.fromEntries(Object.entries(process.env)
    .filter(([key]) => !/^npm_(config|package|lifecycle)_/i.test(key)));
  return {
    ...env,
    npm_config_userconfig: path.join(root, "npmrc"),
    npm_config_cache: path.join(root, "npm-cache"),
    npm_config_update_notifier: "false",
  };
}

function available(command) {
  // Outside this repository, whose packageManager field pnpm would reject.
  return spawnSync(command, ["--version"], { cwd: os.tmpdir(), stdio: "ignore" }).status === 0;
}

function run(executable, ...arguments_) {
  return execFileSync(executable, arguments_, { encoding: "utf8" }).trim();
}

function pack(directory, output) {
  const [entry] = JSON.parse(execFileSync(npm, ["pack", directory, "--json", "--pack-destination", output], {
    encoding: "utf8",
  }));
  return path.join(output, entry.filename);
}

async function writeJson(file, value) {
  await writeFile(file, `${JSON.stringify(value, null, 2)}\n`);
}

// A read-only registry that serves full packuments for both the install and
// the full metadata media types, and tarballs under /-/.
async function startRegistry() {
  const documents = new Map();
  const tarballs = new Map();
  const server = http.createServer((request, response) => {
    const pathname = decodeURIComponent(new URL(request.url, "http://registry").pathname).slice(1);
    const body = pathname.startsWith("-/")
      ? tarballs.get(pathname.slice(2))
      : documents.has(pathname) && Buffer.from(JSON.stringify(documents.get(pathname)));
    if (request.method !== "GET" || !body) {
      response.writeHead(404, { "content-type": "application/json" }).end("{}");
      return;
    }
    response.writeHead(200, {
      "content-type": pathname.startsWith("-/") ? "application/octet-stream" : "application/json",
    }).end(body);
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const url = `http://127.0.0.1:${server.address().port}`;
  return {
    url,
    async publish(tarball, { tag, deprecated } = {}) {
      const manifest = JSON.parse(execFileSync("tar", ["-xOzf", tarball, "package/package.json"], { encoding: "utf8" }));
      const contents = await readFile(tarball);
      const file = path.basename(tarball);
      tarballs.set(file, contents);
      const document = documents.get(manifest.name) ?? { name: manifest.name, "dist-tags": {}, versions: {} };
      document.versions[manifest.version] = {
        ...manifest,
        ...(deprecated ? { deprecated } : {}),
        dist: {
          tarball: `${url}/-/${file}`,
          integrity: `sha512-${createHash("sha512").update(contents).digest("base64")}`,
          shasum: createHash("sha1").update(contents).digest("hex"),
        },
      };
      if (tag) document["dist-tags"][tag] = manifest.version;
      documents.set(manifest.name, document);
    },
    close: () => new Promise((resolve) => server.close(resolve)),
  };
}
