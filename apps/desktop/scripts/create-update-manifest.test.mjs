import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { promisify } from "node:util";
import { artifactName, desktopPackages, desktopTargets, manifestTargets, releaseAssetNames } from "./release-artifacts.mjs";
import { updateFeeds } from "./update-feeds.mjs";

test("every channel feed gets latest.json, and each release's own feed the legacy per-platform files", () => {
  assert.throws(() => manifestTargets({ platforms: { unsupported: {} } }), /unsupported desktop targets: unsupported/);
  assert.throws(() => manifestTargets({ platforms: {} }), /no desktop targets/);
  const entry = (target) => ({ signature: target, url: target });
  const manifest = (version, channel, targets = desktopTargets) => ({ version, channel, platforms: Object.fromEntries(targets.map((target) => [target, entry(target)])) });
  const perPlatform = (release) => Object.fromEntries(["darwin-aarch64", "darwin-x86_64", "windows-x86_64", "windows-aarch64", "linux-x86_64", "linux-aarch64"].map((name) => [
    `latest-${name}.json`, { ...release, platforms: Object.fromEntries(desktopTargets.filter((target) => target.replace(/-(deb|rpm)$/, "") === name).map((target) => [target, entry(target)])) },
  ]));
  const feeds = (release, targets, feed) => Object.fromEntries(updateFeeds(release, targets, feed));
  const beta = manifest("0.3.0-beta.2", "beta");
  assert.deepEqual(manifestTargets(beta), desktopTargets);
  assert.deepEqual(feeds(beta, desktopTargets, "beta"), { "latest.json": beta, ...perPlatform(beta) });
  const stable = manifest("0.3.0", "stable");
  assert.deepEqual(feeds(stable, desktopTargets, "stable"), { "latest.json": stable, ...perPlatform(stable) });
  assert.deepEqual(feeds(stable, desktopTargets, "beta"), { "latest.json": stable });
  // A release missing a target never writes latest.json.
  const partial = manifest("0.3.0-beta.3", "beta", ["darwin-aarch64"]);
  assert.deepEqual(feeds(partial, ["darwin-aarch64"], "beta"), { "latest-darwin-aarch64.json": partial });
});

for (const [channel, version] of [["stable", "0.1.2"], ["beta", "0.1.2-beta.10"]]) {
  test(`${channel} manifests point at the signed packages and the release holds exactly its assets`, async () => {
    const directory = await mkdtemp(path.join(os.tmpdir(), "pap-update-manifest-"));
    const run = () => promisify(execFile)(process.execPath, ["scripts/create-update-manifest.mjs", directory, version, "Dstack-TEE/private-ai-gateway", channel]);
    try {
      const assets = releaseAssetNames(version).filter((name) => name !== "latest.json");
      assert.equal(assets.length + 1, 25);
      for (const file of assets) await writeFile(path.join(directory, file), "fixture");
      for (const specification of desktopPackages) {
        const file = artifactName({ version, ...specification });
        await writeFile(path.join(directory, `${file}.sig`), `${file}-signature\n`);
      }
      await run();
      const text = await readFile(path.join(directory, "latest.json"), "utf8");
      const manifest = JSON.parse(text);
      // Tauri's static manifest format, pretty-printed with a final newline.
      assert.equal(text, `${JSON.stringify({ version, channel, pub_date: manifest.pub_date, platforms: manifest.platforms }, null, 2)}\n`);
      assert.deepEqual(Object.keys(manifest.platforms), desktopTargets);
      assert.equal(manifest.platforms["windows-aarch64"].url, `https://github.com/Dstack-TEE/private-ai-gateway/releases/download/desktop-v${version}/private-ai-proxy-${version}-windows-arm64.exe`);
      assert.equal(manifest.platforms["darwin-x86_64"].signature, `private-ai-proxy-${version}-macos-x64.app.tar.gz-signature`);
      // An App Store package must never be published with the Direct release.
      await writeFile(path.join(directory, `private-ai-proxy-${version}-mac-app-store.pkg`), "fixture");
      await assert.rejects(run, /Unexpected: private-ai-proxy-.*-mac-app-store\.pkg/);
      await rm(path.join(directory, `private-ai-proxy-${version}-mac-app-store.pkg`));
      await rm(path.join(directory, `private-ai-proxy-${version}-linux-x64.rpm.sig`));
      await assert.rejects(run);
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });
}
