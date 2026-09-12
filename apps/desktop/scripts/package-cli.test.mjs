import assert from "node:assert/strict";
import { lstat, mkdir, mkdtemp, readFile, readlink, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { binaries, releaseVersionParts, stageLinuxPackageRoot, stagePortable } from "./package-cli.mjs";

test("stages the three sibling CLI executables and portable alias and Linux package symlinks", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "pap-cli-package-"));
  try {
    const source = path.join(root, "source");
    const portable = path.join(root, "portable");
    const packageRoot = path.join(root, "root");
    await mkdir(source);
    for (const name of binaries) {
      await writeFile(path.join(source, `${name}-x86_64-unknown-linux-gnu`), name);
    }

    await stagePortable({
      sourceDir: source,
      targetTriple: "x86_64-unknown-linux-gnu",
      platform: "linux",
      destination: portable,
    });
    for (const name of binaries) {
      assert.equal(await readFile(path.join(portable, name), "utf8"), name);
    }

    assert.equal(await readlink(path.join(portable, "pap")), "private-ai-proxy");
    const windows = path.join(root, "windows");
    for (const name of binaries) {
      await writeFile(path.join(source, `${name}.exe`), name);
    }
    await stagePortable({ sourceDir: source, targetTriple: "x86_64-pc-windows-msvc", platform: "windows", destination: windows });
    assert.equal(await readFile(path.join(windows, "pap.cmd"), "utf8"), '@echo off\r\n"%~dp0private-ai-proxy.exe" %*\r\n');

    await stageLinuxPackageRoot(portable, packageRoot);
    assert.equal(await readlink(path.join(packageRoot, "usr/bin/private-ai-proxy")), "../libexec/private-ai-proxy/private-ai-proxy");
    assert.equal(await readlink(path.join(packageRoot, "usr/bin/pap")), "../libexec/private-ai-proxy/private-ai-proxy");
    assert.ok((await lstat(path.join(packageRoot, "usr/bin/pap"))).isSymbolicLink());
    for (const name of binaries) {
      assert.equal(
        await readFile(path.join(packageRoot, "usr/libexec/private-ai-proxy", name), "utf8"),
        name,
      );
    }
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("maps prerelease versions to native package ordering", () => {
  assert.deepEqual(releaseVersionParts("1.2.3-beta.4"), {
    deb: "1.2.3~beta.4",
    rpmVersion: "1.2.3",
    rpmRelease: "0.beta.4.1",
  });
  assert.deepEqual(releaseVersionParts("1.2.3"), {
    deb: "1.2.3",
    rpmVersion: "1.2.3",
    rpmRelease: "1",
  });
  assert.throws(() => releaseVersionParts("1.2"), /must be SemVer/);
});
