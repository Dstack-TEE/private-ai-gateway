import assert from "node:assert/strict";
import { test } from "node:test";
import { execFileSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import path from "node:path";
import {
  appVersion,
  publishedRelease,
  releaseChannel,
  shouldAdvance,
  versionRelease,
} from "./release-channel.mjs";
import { runtimeBuildVersion } from "./distribution.mjs";

test("channels require canonical matching versions and release metadata", () => {
  assert.equal(releaseChannel("0.2.0-beta.1").feedTag, "desktop-updates-beta");
  assert.equal(releaseChannel("0.2.0", "stable").feedTag, "desktop-updates-stable");
  for (const version of ["0.2.0", "v0.2.0-beta.1", "0.2.0-beta.0", "0.2.0-beta.01", "0.2.0-rc.1", "0.2.0-beta.1+build", "01.2.0-beta.1"]) assert.throws(() => releaseChannel(version));
  assert.throws(() => releaseChannel("0.2.0-beta.1", "stable"));
  assert.throws(() => releaseChannel("0.2.0", "unknown"));
  assert.throws(() => publishedRelease("desktop-v0.2.0-beta.1", false));
  assert.throws(() => publishedRelease("desktop-v0.2.0", true));
  assert.equal(publishedRelease("desktop-v0.2.0-beta.1", true).channel, "beta");
  assert.equal(versionRelease("0.2.0").channel, "stable");
  assert.equal(versionRelease("0.2.0-beta.3").channel, "beta");
  assert.throws(() => versionRelease("0.2.0-rc.1"));
});

test("runtime build identity distinguishes App Store builds without changing the marketing version", () => {
  assert.equal(runtimeBuildVersion({ APPLE_APP_STORE_BUILD_NUMBER: "11" }), `${appVersion()}+11`);
  assert.equal(runtimeBuildVersion({}), undefined);
});

test("release-please bumps every manifest that carries the app version", async () => {
  const appRoot = new URL("..", import.meta.url);
  const read = async (file) => JSON.parse(await readFile(new URL(file, appRoot), "utf8"));
  const config = (await read("release-please-config.json")).packages["apps/desktop"];
  const version = (await read(".release-please-manifest.json"))["apps/desktop"];
  const files = config["extra-files"].map((file) => `${file.path} ${file.jsonpath}`);
  // Cargo checks Cargo.lock itself: every build runs with --locked.
  const metadata = JSON.parse(execFileSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], { cwd: appRoot, encoding: "utf8" }));
  assert.equal(appVersion(), version);
  assert.equal((await read("package.json")).version, version);
  for (const crate of metadata.packages) {
    const manifest = path.relative(metadata.workspace_root, crate.manifest_path).split(path.sep).join("/");
    assert.equal(crate.version, version, crate.name);
    assert.ok(files.includes(`${manifest} $.package.version`), manifest);
    assert.ok(files.includes(`Cargo.lock $.package[?(@.name.value == '${crate.name}')].version`), crate.name);
  }
});

test("feeds advance using SemVer; the beta feed also carries stable releases", () => {
  assert.equal(shouldAdvance("0.2.0-beta.10", "0.2.0-beta.2", "beta"), true);
  assert.equal(shouldAdvance("0.2.0-beta.2", "0.2.0-beta.10", "beta"), false);
  assert.equal(shouldAdvance("0.2.0-beta.2", "0.2.0-beta.2", "beta"), false);
  assert.equal(shouldAdvance("0.2.0-beta.1", undefined, "beta"), true);
  assert.equal(shouldAdvance("0.2.0", "0.1.9", "stable"), true);
  assert.equal(shouldAdvance("0.2.0", "0.2.0-beta.10", "beta"), true);
  assert.equal(shouldAdvance("0.2.1-beta.1", "0.2.0", "beta"), true);
  assert.equal(shouldAdvance("0.1.9", "0.2.0-beta.1", "beta"), false);
  assert.throws(() => shouldAdvance("0.2.0", "0.2.0-beta.1", "stable"));
  assert.throws(() => shouldAdvance("0.2.0-beta.1", undefined, "stable"));
  assert.throws(() => shouldAdvance("0.2.0-rc.1", undefined, "beta"));
});
