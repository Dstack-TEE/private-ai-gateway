import assert from "node:assert/strict";
import { test } from "node:test";
import {
  publishedRelease,
  releaseChannel,
  releaseTitle,
  shouldAdvance,
  validateReleaseRequest,
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
  assert.equal(releaseTitle("0.2.0", "stable"), "Private AI Proxy v0.2.0");
});

test("runtime build identity distinguishes App Store builds without changing the marketing version", () => {
  assert.equal(runtimeBuildVersion({ DESKTOP_RELEASE_VERSION: "0.1.4", APPLE_APP_STORE_BUILD_NUMBER: "11" }), "0.1.4+11");
  assert.equal(runtimeBuildVersion({ DESKTOP_RELEASE_VERSION: "0.1.4" }), "0.1.4");
  assert.equal(runtimeBuildVersion({ PAP_BUILD_VERSION: "local-build", DESKTOP_RELEASE_VERSION: "0.1.4", APPLE_APP_STORE_BUILD_NUMBER: "11" }), "local-build");
});

test("stable release requests come from main and cover every platform", () => {
  assert.equal(validateReleaseRequest({ version: "", publish: false }), undefined);
  assert.throws(() => validateReleaseRequest({ version: "", publish: true }), /requires a release version/);
  assert.throws(() => validateReleaseRequest({ version: "0.2.0-beta.1", packageOnly: true }), /package_only/);
  assert.throws(() => validateReleaseRequest({ version: "0.2.0", channel: "stable", ref: "refs/heads/feature", platforms: "all" }), /from main/);
  assert.throws(() => validateReleaseRequest({ version: "0.2.0", channel: "stable", ref: "refs/heads/main", platforms: "macos-arm64" }), /every supported platform/);
  assert.throws(() => validateReleaseRequest({ version: "0.2.0", channel: "stable", ref: "refs/heads/main", platforms: "all" }), /release summary/);
  assert.throws(() => validateReleaseRequest({ version: "0.2.0-beta.1", summary: "## Heading" }), /must not contain Markdown headings/);
  assert.throws(
    () => validateReleaseRequest({ version: "0.2.0-beta.1", channel: "beta", ref: "refs/heads/feature", platforms: "all", publish: true }),
    /Published releases must be built from main/,
  );
  assert.throws(
    () => validateReleaseRequest({ version: "0.2.0-beta.1", channel: "beta", ref: "refs/heads/main", platforms: "macos-arm64", publish: true }),
    /Published releases must include every supported platform/,
  );
  assert.equal(
    validateReleaseRequest({ version: "0.2.0", channel: "stable", ref: "refs/heads/main", platforms: "all", publish: true, summary: "- Initial stable release" }).tag,
    "desktop-v0.2.0",
  );
  assert.equal(
    validateReleaseRequest({ version: "0.2.0-beta.1", channel: "beta", ref: "refs/heads/feature", platforms: "macos-arm64" }).tag,
    "desktop-v0.2.0-beta.1",
  );
  assert.equal(
    validateReleaseRequest({ version: "0.2.0-beta.1", channel: "beta", ref: "refs/heads/main", platforms: "all", publish: true }).tag,
    "desktop-v0.2.0-beta.1",
  );
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
