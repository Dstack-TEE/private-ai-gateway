import assert from "node:assert/strict";
import { test } from "node:test";
import { releaseChannel, publishedRelease, shouldAdvance } from "./release-channel.mjs";

test("channels require canonical matching versions and release metadata", () => {
  assert.equal(releaseChannel("0.2.0-beta.1").feedTag, "desktop-updates-beta");
  assert.equal(releaseChannel("0.2.0", "stable").feedTag, "desktop-updates-stable");
  for (const version of ["0.2.0", "v0.2.0-beta.1", "0.2.0-beta.0", "0.2.0-beta.01", "0.2.0-rc.1", "0.2.0-beta.1+build", "01.2.0-beta.1"]) assert.throws(() => releaseChannel(version));
  assert.throws(() => releaseChannel("0.2.0-beta.1", "stable"));
  assert.throws(() => releaseChannel("0.2.0", "unknown"));
  assert.throws(() => publishedRelease("desktop-v0.2.0-beta.1", false));
  assert.throws(() => publishedRelease("desktop-v0.2.0", true));
  assert.equal(publishedRelease("desktop-v0.2.0-beta.1", true).channel, "beta");
});

test("feeds advance using SemVer without downgrade or cross-channel contamination", () => {
  assert.equal(shouldAdvance("0.2.0-beta.10", "0.2.0-beta.2", "beta"), true);
  assert.equal(shouldAdvance("0.2.0-beta.2", "0.2.0-beta.10", "beta"), false);
  assert.equal(shouldAdvance("0.2.0-beta.2", "0.2.0-beta.2", "beta"), false);
  assert.equal(shouldAdvance("0.2.0-beta.1", undefined, "beta"), true);
  assert.equal(shouldAdvance("0.2.0", "0.1.9", "stable"), true);
  assert.throws(() => shouldAdvance("0.2.0-beta.1", "0.1.9", "beta"));
  assert.throws(() => shouldAdvance("0.2.0", "0.2.0-beta.1", "stable"));
});
