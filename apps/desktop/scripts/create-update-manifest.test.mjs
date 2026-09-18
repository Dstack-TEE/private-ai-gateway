import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { promisify } from "node:util";
import { artifactName, desktopPackages, desktopTargets, manifestTargets } from "./release-artifacts.mjs";
import { updateFeeds } from "./update-feeds.mjs";

test("partial releases preserve independent platform versions and the complete legacy feed", () => {
  const targets = desktopTargets;
  const manifest = { version: "0.1.2-beta.38", channel: "beta", platforms: Object.fromEntries(targets.map((target) => [target, { url: target, signature: target }])) };
  assert.deepEqual(manifestTargets(manifest), targets);
  assert.throws(() => manifestTargets({ platforms: { unsupported: {} } }), /unsupported desktop targets: unsupported/);
  assert.throws(() => manifestTargets({ platforms: {} }), /no desktop targets/);
  const complete = updateFeeds(manifest, targets);
  assert.equal(complete.size, 7);
  assert.deepEqual(complete.get("latest.json"), manifest);
  assert.deepEqual(Object.keys(complete.get("latest-linux-aarch64.json").platforms), ["linux-aarch64-deb", "linux-aarch64-rpm"]);
  const partial = updateFeeds({ ...manifest, version: "0.1.2-beta.39" }, ["darwin-aarch64"]);
  assert.deepEqual([...partial.keys()], ["latest-darwin-aarch64.json"]);
  const published = new Map([...complete, ...partial]);
  assert.equal(published.get("latest-darwin-aarch64.json").version, "0.1.2-beta.39");
  assert.equal(published.get("latest-linux-aarch64.json").version, "0.1.2-beta.38");
  assert.equal(published.get("latest.json").version, "0.1.2-beta.38");
});

for (const [channel, version] of [["stable", "0.1.2"], ["beta", "0.1.2-beta.10"]]) {
test(`${channel} manifests use signed platform artifacts and reject incomplete releases`, async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "pap-update-manifest-"));
  const run = () => promisify(execFile)(process.execPath, ["scripts/create-update-manifest.mjs", directory, version, "Dstack-TEE/private-ai-gateway", channel]);
  const selectAssets = () => promisify(execFile)(process.execPath, ["scripts/release-assets.mjs", directory]);
  try {
    for (const specification of desktopPackages) {
      const file = artifactName({ version, ...specification });
      await writeFile(path.join(directory, file), "fixture");
      await writeFile(path.join(directory, `${file}.sig`), `${file}-signature\n`);
    }
    await writeFile(path.join(directory, `private-ai-proxy-cli-${version}-linux-x64.deb`), "cli fixture");
    await writeFile(path.join(directory, `private-ai-proxy-cli-${version}-linux-x64.rpm`), "cli fixture");
    await run();
    const manifest = JSON.parse(await readFile(path.join(directory, "latest.json"), "utf8"));
    assert.equal(manifest.version, version);
    assert.equal(manifest.channel, channel);
    assert.deepEqual(Object.keys(manifest.platforms).sort(), [
      "darwin-aarch64",
      "darwin-x86_64",
      "linux-aarch64-deb",
      "linux-aarch64-rpm",
      "linux-x86_64-deb",
      "linux-x86_64-rpm",
      "windows-aarch64",
      "windows-x86_64",
    ]);
    assert.ok(manifest.platforms["windows-x86_64"].url.endsWith(`/desktop-v${version}/private-ai-proxy-${version}-windows-x64.exe`));
    assert.ok(manifest.platforms["windows-aarch64"].url.endsWith(`/desktop-v${version}/private-ai-proxy-${version}-windows-arm64.exe`));
    assert.ok(manifest.platforms["darwin-aarch64"].url.endsWith(`-macos-arm64.app.tar.gz`));
    assert.ok(manifest.platforms["darwin-x86_64"].url.endsWith(`-macos-x64.app.tar.gz`));
    assert.notEqual(manifest.platforms["darwin-aarch64"].signature, manifest.platforms["darwin-x86_64"].signature);
    for (const entry of Object.values(manifest.platforms)) {
      const filename = path.basename(new URL(entry.url).pathname);
      assert.equal(await readFile(path.join(directory, filename), "utf8"), "fixture");
      assert.equal((await readFile(path.join(directory, `${filename}.sig`), "utf8")).trim(), entry.signature);
    }
    await writeFile(path.join(directory, "duplicate-app.zip"), "duplicate");
    for (const arch of ["arm64", "x64"]) {
      await writeFile(path.join(directory, `private-ai-proxy-${version}-macos-${arch}.dmg`), "disk image");
    }
    await writeFile(path.join(directory, `private-ai-proxy-cli-${version}-linux-x64.tar.gz`), "cli archive");
    const selected = (await selectAssets()).stdout.split("\0").filter(Boolean).map((file) => path.basename(file));
    for (const entry of Object.values(manifest.platforms)) {
      assert.ok(selected.includes(path.basename(new URL(entry.url).pathname)));
    }
    assert.ok(selected.includes(`private-ai-proxy-cli-${version}-linux-x64.tar.gz`));
    assert.ok(selected.includes(`private-ai-proxy-cli-${version}-linux-x64.deb`));
    assert.ok(selected.includes(`private-ai-proxy-cli-${version}-linux-x64.rpm`));
    assert.ok(selected.includes("SHA256SUMS"));
    for (const arch of ["arm64", "x64"]) {
      assert.ok(selected.includes(`private-ai-proxy-${version}-macos-${arch}.dmg`));
      assert.ok((await readFile(path.join(directory, "SHA256SUMS"), "utf8")).includes(`  private-ai-proxy-${version}-macos-${arch}.dmg\n`));
    }
    assert.ok(!selected.some((file) => file.endsWith(".sig") || file === "duplicate-app.zip"));
    await writeFile(path.join(directory, "private-ai-proxy-cli-9.9.9-linux-x64.deb"), "wrong version");
    await assert.rejects(selectAssets(), /does not match version/);
    await rm(path.join(directory, `private-ai-proxy-${version}-linux-x64.rpm.sig`));
    await assert.rejects(run);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
}

test("native ARM64 artifacts are selected by matrix provenance and normalized without changing bytes", async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "pap-native-manifest-"));
  const version = "0.1.2-beta.36";
  const paths = [];
  try {
    for (const specification of desktopPackages.filter(entry => entry.platform !== "macos")) {
      const folder = path.join(directory, `private-ai-proxy-beta-${version}-${"a".repeat(40)}-${specification.platform}-${specification.arch}`);
      await mkdir(folder, { recursive: true });
      const file = path.join(folder, `Private AI Proxy_native${specification.suffix}`);
      await writeFile(file, `${specification.platform}-${specification.arch}`);
      await writeFile(`${file}.sig`, `${specification.platform}-${specification.arch}-signature`);
      paths.push(path.join(folder, artifactName({ version, ...specification })));
    }
    const run = () => promisify(execFile)(process.execPath, ["scripts/create-update-manifest.mjs", directory, version, "Dstack-TEE/private-ai-gateway", "beta", "windows-arm64,windows-x64,linux-arm64,linux-x64"]);
    await run();
    const manifest = JSON.parse(await readFile(path.join(directory, "latest.json"), "utf8"));
    assert.equal(Object.keys(manifest.platforms).length, 6);
    for (const file of paths) {
      const bytes = await readFile(file, "utf8");
      assert.equal(await readFile(`${file}.sig`, "utf8"), `${bytes}-signature`);
    }
    await run();
    await rm(`${paths[0]}.sig`);
    await assert.rejects(run);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
