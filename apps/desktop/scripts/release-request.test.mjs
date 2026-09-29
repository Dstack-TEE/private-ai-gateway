import assert from "node:assert/strict";
import { test } from "node:test";
import { appStoreVersion, desktopRelease, npmRelease } from "./release-request.mjs";

test("only a release tag's push resolves a channel and an App Store build number", () => {
  const tag = (version, overrides = {}) => desktopRelease({ version, event: "push", ref: "refs/tags/desktop-v0.3.0", refName: "desktop-v0.3.0", packageOnly: false, runNumber: 4, ...overrides });
  assert.deepEqual(tag("0.3.0"), { version: "0.3.0", channel: "stable", app_store_build_number: "104" });
  assert.deepEqual(tag("0.3.0-beta.2", { ref: "refs/tags/desktop-v0.3.0-beta.2", refName: "desktop-v0.3.0-beta.2" }), { version: "0.3.0-beta.2", channel: "beta", app_store_build_number: "104" });
  assert.throws(() => tag("0.3.1"), /desktop-v0\.3\.0 must fully verify the committed version 0\.3\.1/);
  assert.throws(() => tag("0.3.0", { packageOnly: true }), /must fully verify/);
  // Manual runs, at a tag or not, build test packages.
  assert.deepEqual(desktopRelease({ version: "0.3.0", event: "workflow_dispatch", ref: "refs/tags/desktop-v0.3.0", refName: "desktop-v0.3.0", packageOnly: true, runNumber: 9 }), { version: "0.3.0" });
  assert.deepEqual(desktopRelease({ version: "0.3.0", event: "workflow_dispatch", ref: "refs/heads/main", refName: "main", packageOnly: false, runNumber: 9 }), { version: "0.3.0" });
});

test("App Store uploads need the committed stable version's tag; other runs package the stable version ahead", () => {
  const upload = { committed: "0.3.0", upload: true, ref: "refs/tags/desktop-v0.3.0", buildNumber: "104" };
  assert.equal(appStoreVersion(upload), "0.3.0");
  assert.throws(() => appStoreVersion({ ...upload, ref: "refs/heads/main" }), /only from the release tag/);
  assert.throws(() => appStoreVersion({ ...upload, committed: "0.3.0-beta.1", ref: "refs/tags/desktop-v0.3.0-beta.1" }), /Stable versions/);
  assert.throws(() => appStoreVersion({ ...upload, buildNumber: "1.2.3.4" }), /build number/);
  const validate = { upload: false, ref: "refs/heads/main", buildNumber: "99999.12" };
  assert.equal(appStoreVersion({ ...validate, committed: "0.2.0-beta.10" }), "0.2.0");
  assert.equal(appStoreVersion({ ...validate, committed: "0.2.1" }), "0.2.1");
  assert.throws(() => appStoreVersion({ ...validate, committed: "0.2.1", buildNumber: undefined }), /build number/);
});

test("npm packages published desktop releases and publishes only from main or their tag", () => {
  const release = (tag) => ({ tag, release: { isDraft: false, tagName: tag }, publish: false, ref: "refs/heads/feature" });
  assert.deepEqual(npmRelease(release("desktop-v0.3.0")), { version: "0.3.0", dist_tag: "latest" });
  assert.deepEqual(npmRelease(release("desktop-v0.3.0-beta.2")), { version: "0.3.0-beta.2", dist_tag: "beta" });
  assert.throws(() => npmRelease({ ...release("desktop-v0.3.0"), release: { isDraft: true, tagName: "desktop-v0.3.0" } }), /published desktop-v\*/);
  assert.throws(() => npmRelease({ ...release("desktop-v0.3.0"), release: { isDraft: false, tagName: "desktop-v0.3.1" } }), /published desktop-v\*/);
  assert.throws(() => npmRelease(release("v0.3.0")), /published desktop-v\*/);
  assert.throws(() => npmRelease(release("desktop-v0.3.0+build")), /without build metadata/);
  assert.throws(() => npmRelease({ ...release("desktop-v0.3.0"), publish: true }), /from main or the matching release tag/);
  for (const ref of ["refs/heads/main", "refs/tags/desktop-v0.3.0"]) {
    assert.equal(npmRelease({ ...release("desktop-v0.3.0"), publish: true, ref }).dist_tag, "latest");
  }
});
