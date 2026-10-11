// Installs the npm packages from a local registry with real package managers:
// npm always, pnpm and bun when they are on PATH. The registry is an
// in-process static server so the test needs no network or credentials.
import assert from "node:assert/strict";
import { execFile, execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, mkdir, mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
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

// The fake native executables are shell scripts.
const supportedHost = ["linux", "darwin"].includes(process.platform) && npmArchitectures.includes(process.arch);
const hostAlias = `private-ai-proxy-${process.platform}-${process.arch}`;
const npm = "npm";
// Installs must not block the event loop, which serves the local registry.
const execFileAsync = promisify(execFile);
const installTimeout = 120_000;

test("npm, pnpm and bun install the wrapper with only the host platform version", { skip: !supportedHost }, async (t) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "pap-npm-install-"));
  const registry = await startRegistry();
  try {
    const packages = path.join(root, "packages");
    // An earlier stable release, installed globally and upgraded below.
    await publishRelease(registry, await buildRelease(root, "0.9.0", packages), "latest");
    const installed = await npmInstall(root, "upgrade", ["private-ai-proxy"], registry.url);
    assert.equal(run(installed.bin("pap"), "--version"), "private-ai-proxy 0.9.0");

    // Before a release's platform versions are on the registry, the release
    // job installs the local tarballs together, as the workflow smoke test
    // does.
    const beta = await buildRelease(root, "1.0.0-beta.2", packages);
    await installLocalTarballs(root, "unpublished-beta", beta, registry.url);
    await publishRelease(registry, beta, "beta");
    const channel = await npmInstall(root, "beta", ["private-ai-proxy@beta"], registry.url);
    assert.equal(run(channel.bin("private-ai-proxy"), "--version"), "private-ai-proxy 1.0.0-beta.2");
    assert.deepEqual(await readdir(channel.modules), [hostAlias]);

    const stable = await buildRelease(root, "1.0.0", packages);
    await installLocalTarballs(root, "unpublished-stable", stable, registry.url);
    await publishRelease(registry, stable, "latest");
    await npmInstall(root, "upgrade", ["private-ai-proxy@latest"], registry.url);
    for (const command of ["private-ai-proxy", "pap", "aci"]) {
      assert.equal(run(installed.bin(command), "--version"), "private-ai-proxy 1.0.0");
    }
    assert.deepEqual(await readdir(installed.modules), [hostAlias]);
    const payload = JSON.parse(await readFile(path.join(installed.modules, hostAlias, "package.json"), "utf8"));
    assert.equal(payload.version, `1.0.0-${process.platform}-${process.arch}`);

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
  const platforms = [];
  for (const platform of Object.keys(npmPlatforms)) {
    for (const arch of npmArchitectures) {
      const tarball = await buildPlatformPackage({ platform, arch, version, source, output });
      platforms.push({ target: `${npmPlatforms[platform]}-${arch}`, tarball });
    }
  }
  const wrapper = await buildWrapperPackage({ version, output });
  return { version, platforms, wrapper };
}

// Publishes in the workflow's order and with its dist-tags.
async function publishRelease(registry, release, channelTag) {
  for (const { target, tarball } of release.platforms) {
    await registry.publish(tarball, channelTag === "latest" ? target : `${channelTag}-${target}`);
  }
  await registry.publish(release.wrapper, channelTag);
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
  const host = release.platforms.find(({ target }) => target === `${process.platform}-${process.arch}`);
  const installed = await npmInstall(root, name, [
    `${hostAlias}@file:${host.tarball}`,
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
    async publish(tarball, tag) {
      const manifest = JSON.parse(execFileSync("tar", ["-xOzf", tarball, "package/package.json"], { encoding: "utf8" }));
      const contents = await readFile(tarball);
      const file = path.basename(tarball);
      tarballs.set(file, contents);
      const document = documents.get(manifest.name) ?? { name: manifest.name, "dist-tags": {}, versions: {} };
      document.versions[manifest.version] = {
        ...manifest,
        dist: {
          tarball: `${url}/-/${file}`,
          integrity: `sha512-${createHash("sha512").update(contents).digest("base64")}`,
          shasum: createHash("sha1").update(contents).digest("hex"),
        },
      };
      document["dist-tags"][tag] = manifest.version;
      documents.set(manifest.name, document);
    },
    close: () => new Promise((resolve) => server.close(resolve)),
  };
}
