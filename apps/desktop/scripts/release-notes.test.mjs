import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";

import { releaseAssetNames } from "./release-artifacts.mjs";
import { releaseSections, withReleaseSections } from "./release-notes.mjs";

const repository = "Dstack-TEE/private-ai-gateway";
const changelog = "## [0.3.0](https://example.test/compare) (2026-09-29)\n\n### Features\n\n* **desktop:** example\n";

async function withRelease(version, channel, run) {
  const directory = await mkdtemp(path.join(os.tmpdir(), "pap-release-notes-"));
  try {
    for (const name of [...releaseAssetNames(version), "SHA256SUMS"]) await writeFile(path.join(directory, name), name);
    await writeFile(path.join(directory, "latest.json"), JSON.stringify({ version, channel }));
    await run(directory);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}

test("download links lead the notes and integrity details close them", async () => {
  await withRelease("0.3.0", "stable", async (directory) => {
    const notes = withReleaseSections(changelog, await releaseSections(directory, repository, "a".repeat(40)));
    assert.ok(notes.startsWith("<!-- desktop-downloads -->\n\n## Downloads\n"));
    assert.ok(notes.indexOf("</details>") < notes.indexOf(changelog.trim()));
    assert.ok(notes.indexOf(changelog.trim()) < notes.indexOf("## Integrity and updates"));
    for (const [earlier, later] of [["macOS Apple Silicon", "macOS Intel"], ["macOS Intel", "Windows x64"], ["Windows ARM64", "Linux x64"], ["Linux x64", "Linux ARM64"]]) {
      assert.ok(notes.indexOf(`| ${earlier} |`) < notes.indexOf(`| ${later} |`), `${earlier} before ${later}`);
    }
    assert.match(notes, /\| macOS Apple Silicon \| \[DMG\]\(https:\/\/github\.com\/Dstack-TEE\/private-ai-gateway\/releases\/download\/desktop-v0\.3\.0\/private-ai-proxy-0\.3\.0-macos-arm64\.dmg\) \|/);
    assert.match(notes, /\| Linux x64 \| \[DEB\]\([^)]+-linux-x64\.deb\) · \[RPM\]\([^)]+-linux-x64\.rpm\) · \[ARCH\]\([^)]+-linux-x64\.pkg\.tar\.zst\) \|/);
    assert.match(notes, /Also on the \[Mac App Store\]/);
    assert.match(notes, /<summary>Standalone CLI downloads<\/summary>/);
    assert.match(notes, /\| Windows x64 \| \[ZIP\]\([^)]+private-ai-proxy-cli-0\.3\.0-windows-x64\.zip\) \|/);
    assert.match(notes, /\| Linux ARM64 \| \[TAR\.GZ\].*\[DEB\].*\[RPM\].*\[ARCH\]/);
    assert.match(notes, /\[SHA-256 checksums\]\([^)]+\/desktop-v0\.3\.0\/SHA256SUMS\)/);
    assert.match(notes, /Developer ID signed and notarized/);
    assert.match(notes, /Build commit: \[`aaaaaaa`\]/);
    // Updater packages are not offered as installers.
    assert.doesNotMatch(notes, /\.app\.tar\.gz\)/);
  });
});

test("a rerun replaces the sections, including one appended by an earlier layout", async () => {
  await withRelease("0.3.0", "stable", async (directory) => {
    const once = withReleaseSections(changelog, await releaseSections(directory, repository, "a".repeat(40)));
    const again = withReleaseSections(once, await releaseSections(directory, repository, "b".repeat(40)));
    assert.equal(again.match(/## Downloads/g)?.length, 1);
    assert.equal(again.match(/## Integrity and updates/g)?.length, 1);
    assert.equal(again.split(changelog.trim()).length, 2);
    assert.match(again, /Build commit: \[`bbbbbbb`\]/);
    // v0.3.0's first edit appended both parts in one block after the changelog.
    const appended = `${changelog}\n<!-- desktop-downloads -->\n\n## Downloads\n\n## Integrity and updates\n\n<!-- /desktop-downloads -->\n`;
    assert.equal(withReleaseSections(appended, await releaseSections(directory, repository, "a".repeat(40))), once);
  });
});

test("beta releases, which skip the App Store, do not link it", async () => {
  await withRelease("0.3.0-beta.1", "beta", async (directory) => {
    const { downloads } = await releaseSections(directory, repository, "c".repeat(40));
    assert.match(downloads, /desktop-v0\.3\.0-beta\.1\/private-ai-proxy-0\.3\.0-beta\.1-macos-x64\.dmg/);
    assert.doesNotMatch(downloads, /Mac App Store/);
  });
});

test("the sections need a repository and the full build commit", async () => {
  await withRelease("0.3.0", "stable", async (directory) => {
    await assert.rejects(releaseSections(directory, "not a repository", "a".repeat(40)), /repository/);
    await assert.rejects(releaseSections(directory, repository, "abc1234"), /40-character/);
  });
});
