import assert from "node:assert/strict";
import { test } from "node:test";
import { appStoreVersion, desktopRelease, npmRelease } from "./release-request.mjs";

test("release requests resolve only from their release tags", () => {
  const tag = { event: "push", ref: "refs/tags/desktop-v0.3.0", refName: "desktop-v0.3.0", packageOnly: false, runNumber: 4 };
  assert.deepEqual(desktopRelease({ ...tag, version: "0.3.0" }), { version: "0.3.0", channel: "stable", app_store_build_number: "104" });
  assert.deepEqual(desktopRelease({ ...tag, version: "0.3.0", event: "workflow_dispatch", packageOnly: true }), { version: "0.3.0" });
  for (const bad of [{ version: "0.3.1" }, { version: "0.3.0", packageOnly: true }]) assert.throws(() => desktopRelease({ ...tag, ...bad }), /must fully verify/);

  const upload = { committed: "0.3.0", upload: true, ref: "refs/tags/desktop-v0.3.0", buildNumber: "104" };
  assert.equal(appStoreVersion(upload), "0.3.0");
  assert.equal(appStoreVersion({ committed: "0.2.0-beta.10", upload: false, ref: "refs/heads/main", buildNumber: "99999.12" }), "0.2.0");
  for (const bad of [{ ref: "refs/heads/main" }, { committed: "0.3.0-beta.1", ref: "refs/tags/desktop-v0.3.0-beta.1" }, { buildNumber: "1.2.3.4" }]) assert.throws(() => appStoreVersion({ ...upload, ...bad }));

  const npm = (tag, overrides = {}) => npmRelease({ tag, release: { isDraft: false, tagName: tag }, publish: false, ref: "refs/heads/feature", ...overrides });
  assert.deepEqual(npm("desktop-v0.3.0"), { version: "0.3.0", dist_tag: "latest" });
  assert.deepEqual(npm("desktop-v0.3.0-beta.2", { publish: true, ref: "refs/heads/main" }), { version: "0.3.0-beta.2", dist_tag: "beta" });
  for (const [tag, bad] of [["desktop-v0.3.0", { publish: true }], ["desktop-v0.3.0", { release: { isDraft: true, tagName: "desktop-v0.3.0" } }], ["v0.3.0", {}]]) assert.throws(() => npm(tag, bad));
});
