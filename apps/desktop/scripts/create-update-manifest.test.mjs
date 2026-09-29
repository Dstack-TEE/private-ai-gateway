import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { promisify } from "node:util";
import { artifactName, desktopPackages, desktopTargets, manifestTargets, releaseAssetNames } from "./release-artifacts.mjs";
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

test("feed files keep their names and contents", () => {
  const entry = (target) => ({ signature: `sig-${target}`, url: `https://example.test/${target}` });
  const manifest = (version, channel, targets) => ({ version, channel, pub_date: "2026-01-02T03:04:05.000Z", platforms: Object.fromEntries(targets.map((target) => [target, entry(target)])) });
  const beta = manifest("0.3.0-beta.2", "beta", desktopTargets);
  const perPlatform = (release) => ({
    "latest-darwin-aarch64.json": { ...release, platforms: { "darwin-aarch64": entry("darwin-aarch64") } },
    "latest-darwin-x86_64.json": { ...release, platforms: { "darwin-x86_64": entry("darwin-x86_64") } },
    "latest-windows-x86_64.json": { ...release, platforms: { "windows-x86_64": entry("windows-x86_64") } },
    "latest-windows-aarch64.json": { ...release, platforms: { "windows-aarch64": entry("windows-aarch64") } },
    "latest-linux-x86_64.json": { ...release, platforms: { "linux-x86_64-deb": entry("linux-x86_64-deb"), "linux-x86_64-rpm": entry("linux-x86_64-rpm") } },
    "latest-linux-aarch64.json": { ...release, platforms: { "linux-aarch64-deb": entry("linux-aarch64-deb"), "linux-aarch64-rpm": entry("linux-aarch64-rpm") } },
  });
  assert.deepEqual(Object.fromEntries(updateFeeds(beta, desktopTargets, "beta")), { "latest.json": beta, ...perPlatform(beta) });
  const stable = manifest("0.3.0", "stable", desktopTargets);
  assert.deepEqual(Object.fromEntries(updateFeeds(stable, desktopTargets, "stable")), { "latest.json": stable, ...perPlatform(stable) });
  assert.deepEqual(Object.fromEntries(updateFeeds(stable, desktopTargets, "beta")), { "latest.json": stable });
  // A release missing a target never writes latest.json.
  const linux = ["linux-x86_64-deb", "linux-x86_64-rpm"];
  const partial = manifest("0.3.0-beta.3", "beta", linux);
  assert.deepEqual(Object.fromEntries(updateFeeds(partial, linux, "beta")), {
    "latest-linux-x86_64.json": { ...partial, platforms: { "linux-x86_64-deb": entry("linux-x86_64-deb"), "linux-x86_64-rpm": entry("linux-x86_64-rpm") } },
  });
  assert.deepEqual(Object.fromEntries(updateFeeds({ ...partial, channel: "stable", version: "0.3.0" }, linux, "beta")), {});
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
      assert.equal(text, `${JSON.stringify(manifest, null, 2)}\n`);
      assert.deepEqual(Object.keys(manifest), ["version", "channel", "pub_date", "platforms"]);
      assert.equal(new Date(manifest.pub_date).toISOString(), manifest.pub_date);
      assert.equal(manifest.version, version);
      assert.equal(manifest.channel, channel);
      assert.deepEqual(Object.keys(manifest.platforms), desktopTargets);
      for (const [target, entry] of Object.entries(manifest.platforms)) assert.deepEqual(Object.keys(entry), ["signature", "url"], target);
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
