import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { promisify } from "node:util";
import { artifactName, desktopPackages, desktopTargets, manifestTargets } from "./release-artifacts.mjs";
import { updateFeeds } from "./update-feeds.mjs";

test("releases write latest.json to every channel feed and legacy per-platform files to their own", () => {
  const targets = desktopTargets;
  const manifest = { version: "0.1.2-beta.38", channel: "beta", platforms: Object.fromEntries(targets.map((target) => [target, { url: target, signature: target }])) };
  assert.deepEqual(manifestTargets(manifest), targets);
  assert.throws(() => manifestTargets({ platforms: { unsupported: {} } }), /unsupported desktop targets: unsupported/);
  assert.throws(() => manifestTargets({ platforms: {} }), /no desktop targets/);
  const complete = updateFeeds(manifest, targets, "beta");
  assert.equal(complete.size, 7);
  assert.deepEqual(complete.get("latest.json"), manifest);
  assert.deepEqual(Object.keys(complete.get("latest-linux-aarch64.json").platforms), ["linux-aarch64-deb", "linux-aarch64-rpm"]);
  const partial = updateFeeds({ ...manifest, version: "0.1.2-beta.39" }, ["darwin-aarch64"], "beta");
  assert.deepEqual([...partial.keys()], ["latest-darwin-aarch64.json"]);
  const stable = { ...manifest, version: "0.1.2", channel: "stable" };
  assert.equal(updateFeeds(stable, targets, "stable").size, 7);
  assert.deepEqual([...updateFeeds(stable, targets, "beta")], [["latest.json", stable]]);
});

for (const [channel, version] of [["stable", "0.1.2"], ["beta", "0.1.2-beta.10"]]) {
  test(`${channel} manifests point at the signed packages and reject incomplete releases`, async () => {
    const directory = await mkdtemp(path.join(os.tmpdir(), "pap-update-manifest-"));
    const run = () => promisify(execFile)(process.execPath, ["scripts/create-update-manifest.mjs", directory, version, "Dstack-TEE/private-ai-gateway", channel]);
    try {
      for (const specification of desktopPackages) {
        const file = artifactName({ version, ...specification });
        await writeFile(path.join(directory, file), "fixture");
        await writeFile(path.join(directory, `${file}.sig`), `${file}-signature\n`);
      }
      await run();
      const manifest = JSON.parse(await readFile(path.join(directory, "latest.json"), "utf8"));
      assert.equal(manifest.version, version);
      assert.equal(manifest.channel, channel);
      assert.deepEqual(Object.keys(manifest.platforms).sort(), [...desktopTargets].sort());
      assert.equal(manifest.platforms["windows-aarch64"].url, `https://github.com/Dstack-TEE/private-ai-gateway/releases/download/desktop-v${version}/private-ai-proxy-${version}-windows-arm64.exe`);
      assert.equal(manifest.platforms["darwin-x86_64"].signature, `private-ai-proxy-${version}-macos-x64.app.tar.gz-signature`);
      await rm(path.join(directory, `private-ai-proxy-${version}-linux-x64.rpm.sig`));
      await assert.rejects(run);
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });
}
