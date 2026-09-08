import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { test } from "node:test";
import { promisify } from "node:util";

for (const [channel, version] of [["stable", "0.1.2"], ["beta", "0.1.2-beta.10"]]) {
test(`${channel} manifests use signed platform artifacts and reject incomplete releases`, async () => {
  await mkdir("playwright-artifacts", { recursive: true });
  const directory = await mkdtemp(path.resolve("playwright-artifacts/update-manifest-"));
  const run = () => promisify(execFile)(process.execPath, ["scripts/create-update-manifest.mjs", directory, version, "Dstack-TEE/private-ai-gateway", channel]);
  try {
    for (const file of [`private-ai-proxy-${version}-macos-arm64.app.tar.gz`, `private-ai-proxy-${version}-macos-x64.app.tar.gz`, "Gateway Setup.exe", "Gateway.deb", "Gateway.rpm"]) {
      await writeFile(path.join(directory, file), "fixture");
      await writeFile(path.join(directory, `${file}.sig`), `${file}-signature\n`);
    }
    await writeFile(path.join(directory, "private-ai-proxy-cli_0.1.2_amd64.deb"), "cli fixture");
    await writeFile(path.join(directory, "private-ai-proxy-cli-0.1.2.x86_64.rpm"), "cli fixture");
    await run();
    const manifest = JSON.parse(await readFile(path.join(directory, "latest.json"), "utf8"));
    assert.equal(manifest.version, version);
    assert.equal(manifest.channel, channel);
    assert.deepEqual(Object.keys(manifest.platforms).sort(), [
      "darwin-aarch64",
      "darwin-x86_64",
      "linux-x86_64-deb",
      "linux-x86_64-rpm",
      "windows-x86_64",
    ]);
    assert.ok(manifest.platforms["windows-x86_64"].url.endsWith(`/desktop-v${version}/private-ai-proxy-${version}-windows-x64.exe`));
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
    await writeFile(path.join(directory, "private-ai-proxy-cli-0.1.2-linux-x64.tar.gz"), "cli archive");
    const selected = (await promisify(execFile)(process.execPath, ["scripts/release-assets.mjs", directory])).stdout.split("\0").filter(Boolean).map((file) => path.basename(file));
    for (const entry of Object.values(manifest.platforms)) {
      assert.ok(selected.includes(path.basename(new URL(entry.url).pathname)));
    }
    assert.ok(selected.includes("private-ai-proxy-cli-0.1.2-linux-x64.tar.gz"));
    assert.ok(selected.includes("SHA256SUMS"));
    for (const arch of ["arm64", "x64"]) {
      assert.ok(selected.includes(`private-ai-proxy-${version}-macos-${arch}.dmg`));
      assert.ok((await readFile(path.join(directory, "SHA256SUMS"), "utf8")).includes(`  private-ai-proxy-${version}-macos-${arch}.dmg\n`));
    }
    assert.ok(!selected.some((file) => file.endsWith(".sig") || file === "duplicate-app.zip" || file.startsWith("private-ai-proxy-cli_")));
    await rm(path.join(directory, `private-ai-proxy-${version}-linux-x64.rpm.sig`));
    await assert.rejects(run);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
}
