import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, readlink, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { assertWebBundle, binaries, stagePortable } from "./package-cli.mjs";

test("requires the web renderer before CLI packaging", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "pap-cli-web-"));
  try {
    await assert.rejects(assertWebBundle(root), /run npm run build:web/);
    await mkdir(path.join(root, "runtime/web-dist"), { recursive: true });
    await writeFile(path.join(root, "runtime/web-dist/index.html"), "<!doctype html>");
    await assert.doesNotReject(assertWebBundle(root));
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("stages the three sibling CLI executables and portable aliases", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "pap-cli-package-"));
  try {
    const source = path.join(root, "source");
    const portable = path.join(root, "portable");
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
    assert.equal(await readlink(path.join(portable, "aci")), "private-ai-proxy");
    const windows = path.join(root, "windows");
    for (const name of binaries) {
      await writeFile(path.join(source, `${name}.exe`), name);
    }
    await stagePortable({ sourceDir: source, targetTriple: "x86_64-pc-windows-msvc", platform: "windows", destination: windows });
    const shim = await readFile(new URL("../cli/manage/alias.cmd", import.meta.url), "utf8");
    assert.equal(await readFile(path.join(windows, "pap.cmd"), "utf8"), shim);
    assert.equal(await readFile(path.join(windows, "aci.cmd"), "utf8"), shim);

  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
