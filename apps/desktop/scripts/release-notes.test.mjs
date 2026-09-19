import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";

import { buildReleaseNotes } from "./release-notes.mjs";

test("release notes use the stable product format and deterministic platform order", async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "pap-release-notes-"));
  const commit = "a".repeat(40);
  try {
    await writeFile(path.join(directory, "latest.json"), JSON.stringify({ version: "0.2.0", channel: "stable" }));
    for (const name of [
      "private-ai-proxy-0.2.0-linux-x64.rpm",
      "private-ai-proxy-0.2.0-macos-arm64.dmg",
      "private-ai-proxy-0.2.0-linux-x64.deb",
      "private-ai-proxy-0.2.0-linux-x64.pkg.tar.zst",
      "private-ai-proxy-0.2.0-windows-x64.exe",
      "private-ai-proxy-cli-0.2.0-macos-arm64.tar.gz",
      "private-ai-proxy-cli-0.2.0-windows-x64.zip",
      "private-ai-proxy-cli-0.2.0-linux-x64.tar.gz",
      "private-ai-proxy-cli-0.2.0-linux-x64.deb",
      "private-ai-proxy-cli-0.2.0-linux-x64.rpm",
      "private-ai-proxy-cli-0.2.0-linux-x64.pkg.tar.zst",
      "SHA256SUMS",
    ]) await writeFile(path.join(directory, name), name);

    const notes = await buildReleaseNotes(directory, "Dstack-TEE/private-ai-gateway", commit, "- Initial stable release");
    assert.match(notes, /^Production release on the stable update channel\./);
    assert.match(notes, /## What's changed\n\n- Initial stable release/);
    assert.ok(notes.indexOf("macOS Apple Silicon") < notes.indexOf("Windows x64"));
    assert.ok(notes.indexOf("[DEB]") < notes.indexOf("[RPM]"));
    assert.ok(notes.indexOf("[RPM]") < notes.indexOf("[ARCH]"));
    assert.match(notes, /<summary>Standalone CLI downloads<\/summary>/);
    assert.match(notes, /Linux x64 \| \[TAR\.GZ\].*\[DEB\].*\[RPM\].*\[ARCH\]/);
    assert.match(notes, /## Integrity and updates/);
    assert.match(notes, /Build commit: \[`aaaaaaa`\]/);
    await assert.rejects(
      buildReleaseNotes(directory, "Dstack-TEE/private-ai-gateway", commit),
      /require a summary/,
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("beta release notes carry an explicit prerelease warning", async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "pap-beta-notes-"));
  try {
    await writeFile(path.join(directory, "latest.json"), JSON.stringify({ version: "0.2.0-beta.1", channel: "beta" }));
    await writeFile(path.join(directory, "SHA256SUMS"), "fixture");
    const notes = await buildReleaseNotes(directory, "Dstack-TEE/private-ai-gateway", "b".repeat(40));
    assert.match(notes, /^> Beta channel build for testing\./);
    assert.match(notes, /## What's changed\n\nTesting build for the linked commit\./);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
