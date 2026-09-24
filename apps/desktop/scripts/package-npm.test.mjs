import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import {
  chmod,
  mkdir,
  mkdtemp,
  rename,
  rm,
  writeFile,
} from "node:fs/promises";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import {
  buildPlatformPackage,
  buildWrapperPackage,
  npmArchitectures,
  npmPlatforms,
  platformManifest,
  platformPackageName,
  wrapperManifest,
} from "./package-npm.mjs";
import { binaries } from "./package-cli.mjs";

const require = createRequire(import.meta.url);
const launcherModule = require("../npm/private-ai-proxy/bin/private-ai-proxy.cjs");
const version = "1.2.3-beta.4";
const targets = Object.entries(npmPlatforms).flatMap(([platform, npmPlatform]) =>
  npmArchitectures.map((arch) => ({ platform, npmPlatform, arch })));

test("pins one scoped platform package per target at the wrapper version", () => {
  const wrapper = wrapperManifest(version);
  assert.deepEqual(wrapper.optionalDependencies, {
    "@phala/private-ai-proxy-darwin-arm64": version,
    "@phala/private-ai-proxy-darwin-x64": version,
    "@phala/private-ai-proxy-linux-arm64": version,
    "@phala/private-ai-proxy-linux-x64": version,
    "@phala/private-ai-proxy-win32-arm64": version,
    "@phala/private-ai-proxy-win32-x64": version,
  });
  assert.equal(wrapper.scripts, undefined, "the wrapper must not rely on install scripts");

  for (const { platform, npmPlatform, arch } of targets) {
    const manifest = platformManifest({ platform, arch, version });
    assert.equal(manifest.name, `@phala/private-ai-proxy-${npmPlatform}-${arch}`);
    assert.equal(manifest.version, version);
    assert.deepEqual(manifest.os, [npmPlatform]);
    assert.deepEqual(manifest.cpu, [arch]);
    assert.deepEqual(manifest.libc, platform === "linux" ? ["glibc"] : undefined);
    assert.equal(manifest.publishConfig.access, "public");
    assert.equal(manifest.scripts, undefined);
  }
  assert.throws(() => platformPackageName("linux", "ia32"), /Unsupported npm package target/);
  assert.throws(() => wrapperManifest("1.2.3+build"), /without build metadata/);
});

test("the launcher resolves the same package the wrapper pins for each target", () => {
  for (const { platform, npmPlatform, arch } of targets) {
    assert.deepEqual(launcherModule.platformPackage(npmPlatform, arch), {
      name: platformPackageName(platform, arch),
      executable: `vendor/private-ai-proxy${npmPlatform === "win32" ? ".exe" : ""}`,
    });
  }
  assert.equal(launcherModule.supportedTargets.length, targets.length);
  for (const [platform, arch] of [["linux", "ia32"], ["freebsd", "x64"], ["darwin", "ppc64"]]) {
    assert.equal(launcherModule.platformPackage(platform, arch), undefined);
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

    const platformTarball = await buildPlatformPackage({ platform: "linux", arch: "x64", version, source, output });
    const wrapperTarball = await buildWrapperPackage({ version, output });
    assert.equal(path.basename(platformTarball), `phala-private-ai-proxy-linux-x64-${version}.tgz`);
    assert.equal(path.basename(wrapperTarball), `private-ai-proxy-${version}.tgz`);
    assert.deepEqual(
      tarballFiles(platformTarball).sort(),
      ["package/LICENSE", "package/README.md", "package/package.json", ...binaries.map((binary) => `package/vendor/${binary}`)].sort(),
    );
    assert.deepEqual(
      tarballFiles(wrapperTarball).sort(),
      ["package/LICENSE", "package/README.md", "package/bin/private-ai-proxy.cjs", "package/package.json"],
    );

    // A wrapper installed with --omit=optional has no platform package.
    const modules = path.join(root, "node_modules");
    const wrapperDirectory = path.join(modules, "private-ai-proxy");
    await extractPackage(wrapperTarball, wrapperDirectory, root);
    const launcher = path.join(wrapperDirectory, "bin/private-ai-proxy.cjs");
    const missing = spawnSync(process.execPath, [launcher], { encoding: "utf8" });
    assert.equal(missing.status, 1);
    assert.match(missing.stderr, /optional dependency @phala\/private-ai-proxy-linux-x64 is not installed/);
    assert.match(missing.stderr, /without --omit=optional/);

    const platformDirectory = path.join(modules, "@phala/private-ai-proxy-linux-x64");
    await extractPackage(platformTarball, platformDirectory, root);
    assert.equal(execFileSync(process.execPath, [launcher, "hello", "world"], { encoding: "utf8" }), "native:hello world\n");
    const failed = spawnSync(process.execPath, [launcher, "--fail"], { encoding: "utf8" });
    assert.equal(failed.status, 7);

    // A platform package from another release must not run.
    await rm(platformDirectory, { recursive: true });
    const otherTarball = await buildPlatformPackage({ platform: "linux", arch: "x64", version: "1.2.3", source, output });
    await extractPackage(otherTarball, platformDirectory, root);
    const mismatched = spawnSync(process.execPath, [launcher], { encoding: "utf8" });
    assert.equal(mismatched.status, 1);
    assert.match(mismatched.stderr, /@phala\/private-ai-proxy-linux-x64@1\.2\.3 does not match private-ai-proxy@1\.2\.3-beta\.4/);
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
