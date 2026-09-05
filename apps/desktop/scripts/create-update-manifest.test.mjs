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
    for (const file of ["Gateway.app.tar.gz", "Gateway Setup.exe", "Gateway.AppImage"]) {
      await writeFile(path.join(directory, file), "fixture");
      await writeFile(path.join(directory, `${file}.sig`), "test-signature\n");
    }
    await run();
    const manifest = JSON.parse(await readFile(path.join(directory, "latest.json"), "utf8"));
    assert.equal(manifest.version, version);
    assert.equal(manifest.channel, channel);
    assert.deepEqual(Object.keys(manifest.platforms).sort(), ["darwin-aarch64", "linux-x86_64", "windows-x86_64"]);
    assert.ok(manifest.platforms["windows-x86_64"].url.endsWith(`/desktop-v${version}/windows-x86_64-${version}.exe`));
    for (const entry of Object.values(manifest.platforms)) {
      const filename = path.basename(new URL(entry.url).pathname);
      assert.equal(await readFile(path.join(directory, filename), "utf8"), "fixture");
      assert.equal((await readFile(path.join(directory, `${filename}.sig`), "utf8")).trim(), entry.signature);
    }
    assert.equal(manifest.platforms["darwin-aarch64"].signature, "test-signature");
    await rm(path.join(directory, `linux-x86_64-${version}.AppImage.sig`));
    await assert.rejects(run);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
}
