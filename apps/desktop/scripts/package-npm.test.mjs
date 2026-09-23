import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import {
  chmod,
  mkdir,
  mkdtemp,
  readFile,
  rename,
  rm,
  writeFile,
} from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import {
  buildPlatformPackage,
  buildWrapperPackage,
  npmPackageName,
  platformPackageAlias,
  platformPackageVersion,
} from "./package-npm.mjs";
import { assertWebBundle, binaries } from "./package-cli.mjs";

const version = "1.2.3-beta.4";

test("npm native packaging shares the CLI web bundle prerequisite", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "pap-npm-web-"));
  try {
    await assert.rejects(assertWebBundle(root), /Missing embedded web UI/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("packs a thin wrapper and a native package that execute together", {
  skip: process.platform !== "linux" || process.arch !== "x64",
}, async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "pap-npm-package-"));
  try {
    const source = path.join(root, "source");
    const output = path.join(root, "output");
    await mkdir(source);
    for (const binary of binaries) {
      const file = path.join(source, binary);
      const body = binary === "private-ai-proxy"
        ? "#!/bin/sh\nif [ \"${1:-}\" = --fail ]; then exit 7; fi\nprintf 'native:%s\\n' \"$*\"\n"
        : `#!/bin/sh\nprintf '${binary}\\n'\n`;
      await writeFile(file, body);
      await chmod(file, 0o755);
    }

    const platformTarball = await buildPlatformPackage({
      platform: "linux",
      arch: "x64",
      version,
      source,
      output,
    });
    const wrapperTarball = await buildWrapperPackage({ version, output });
    const platformAlias = platformPackageAlias("linux", "x64");
    const platformVersion = platformPackageVersion(version, "linux", "x64");
    assert.equal(path.basename(platformTarball), `${npmPackageName}-${platformVersion}.tgz`);
    assert.equal(path.basename(wrapperTarball), `${npmPackageName}-${version}.tgz`);

    const platformFiles = tarballFiles(platformTarball);
    assert.deepEqual(
      platformFiles.filter((file) => file.startsWith("package/vendor/")).sort(),
      binaries.map((binary) => `package/vendor/${binary}`).sort(),
    );
    const wrapperFiles = tarballFiles(wrapperTarball);
    assert.ok(wrapperFiles.includes("package/bin/private-ai-proxy.cjs"));
    assert.ok(!wrapperFiles.some((file) => file.startsWith("package/vendor/")));

    const missingDirectory = path.join(root, "missing/node_modules/private-ai-proxy");
    await extractPackage(wrapperTarball, missingDirectory, root);
    const missing = spawnSync(process.execPath, [path.join(missingDirectory, "bin/private-ai-proxy.cjs")], {
      encoding: "utf8",
    });
    assert.equal(missing.status, 1);
    assert.match(missing.stderr, new RegExp(`${platformAlias} is missing`));
    assert.match(missing.stderr, /without --omit=optional/);

    const install = path.join(root, "install");
    const npm = process.platform === "win32" ? "npm.cmd" : "npm";
    execFileSync(npm, [
      "install",
      "--prefix", install,
      "--ignore-scripts",
      "--no-audit",
      "--no-fund",
      `${platformAlias}@file:${platformTarball}`,
      `file:${wrapperTarball}`,
    ]);

    const wrapperDirectory = path.join(install, "node_modules/private-ai-proxy");
    const platformDirectory = path.join(install, "node_modules", platformAlias);
    const platformManifest = JSON.parse(await readFile(path.join(platformDirectory, "package.json"), "utf8"));
    assert.equal(platformManifest.name, npmPackageName);
    assert.equal(platformManifest.version, platformVersion);
    assert.deepEqual(platformManifest.os, ["linux"]);
    assert.deepEqual(platformManifest.cpu, ["x64"]);
    const wrapperManifest = JSON.parse(await readFile(path.join(wrapperDirectory, "package.json"), "utf8"));
    assert.equal(
      wrapperManifest.optionalDependencies[platformAlias],
      `npm:${npmPackageName}@${platformVersion}`,
    );
    assert.equal(Object.keys(wrapperManifest.optionalDependencies).length, 6);

    const launcher = path.join(wrapperDirectory, "bin/private-ai-proxy.cjs");
    assert.equal(execFileSync(process.execPath, [launcher, "hello", "world"], { encoding: "utf8" }), "native:hello world\n");
    const failed = spawnSync(process.execPath, [launcher, "--fail"], { encoding: "utf8" });
    assert.equal(failed.status, 7);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

function tarballFiles(tarball) {
  return execFileSync("tar", ["-tzf", tarball], { encoding: "utf8" }).trim().split("\n");
}

async function extractPackage(tarball, destination, root) {
  const scratch = await mkdtemp(path.join(root, "extract-"));
  execFileSync("tar", ["-xzf", tarball, "-C", scratch]);
  await mkdir(path.dirname(destination), { recursive: true });
  await rename(path.join(scratch, "package"), destination);
  await rm(scratch, { recursive: true, force: true });
}
