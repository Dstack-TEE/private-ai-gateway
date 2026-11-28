import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { load } from "js-yaml";

import { binaries } from "./package-cli.mjs";
import { artifactName, desktopPackages } from "./release-artifacts.mjs";

const hasZipTools = ["zip", "unzip"].every((command) => spawnSync(command, ["-v"]).status === 0);

test("writes WinGet filenames and scope and retains runtime dependencies for legacy releases", {
  skip: process.platform === "win32" || !hasZipTools,
}, async (t) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "pap-winget-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const version = "1.2.3";
  const tag = `desktop-v${version}`;
  const repository = "fixture/repo";
  const releaseUrl = `https://github.com/${repository}/releases`;
  const assets = [];
  const sums = [];
  for (const entry of desktopPackages.filter(({ platform }) => platform === "windows")) {
    const installer = artifactName({ version, ...entry });
    const archive = artifactName({ version, ...entry, cli: true, suffix: ".zip" });
    await writeFile(path.join(root, installer), installer);
    const directory = path.basename(archive, ".zip");
    await mkdir(path.join(root, directory));
    for (const binary of binaries) {
      await writeFile(path.join(root, directory, `${binary}.exe`), binary);
    }
    execFileSync("zip", ["-qr", archive, directory], { cwd: root });
    for (const name of [installer, archive]) {
      const digest = createHash("sha256").update(await readFile(path.join(root, name))).digest("hex");
      sums.push(`${digest}  ${name}`);
      assets.push({ name, digest: `sha256:${digest}`, browser_download_url: `${releaseUrl}/download/${tag}/${name}` });
    }
  }
  await writeFile(path.join(root, "SHA256SUMS"), sums.join("\n"));
  await writeFile(path.join(root, "release.json"), JSON.stringify({
    tag_name: tag, draft: false, prerelease: false, published_at: "2026-01-01T00:00:00Z",
    html_url: `${releaseUrl}/tag/${tag}`, assets,
  }));
  await writeFile(path.join(root, "gh"), `#!/usr/bin/env node
const { copyFileSync, readFileSync } = require("node:fs");
const path = require("node:path");
const [command, ...args] = process.argv.slice(2);
if (command === "api") {
  process.stdout.write(readFileSync(path.join(__dirname, "release.json")));
} else if (command === "release" && args[0] === "download") {
  const target = args[args.indexOf("--dir") + 1];
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--pattern") {
      const name = args[++i];
      copyFileSync(path.join(__dirname, name), path.join(target, name));
    }
  }
} else {
  throw new Error("Unexpected gh command");
}
`, { mode: 0o755 });

  const output = path.join(root, "output");
  execFileSync(process.execPath, [
    fileURLToPath(new URL("./package-winget.mjs", import.meta.url)), output, repository, tag, "--legacy",
  ], { env: { ...process.env, PATH: `${root}${path.delimiter}${process.env.PATH}` } });
  for (const id of ["DstackTEE.PrivateAIProxy", "DstackTEE.PrivateAIProxy.CLI"]) {
    const directory = path.join(output, "manifests", "d", ...id.split("."), version);
    assert.deepEqual((await readdir(directory)).sort(), [
      `${id}.installer.yaml`, `${id}.locale.en-US.yaml`, `${id}.yaml`,
    ]);
    const manifest = load(await readFile(path.join(directory, `${id}.installer.yaml`), "utf8"));
    assert.equal(manifest.Scope, id.endsWith(".CLI") ? undefined : "user");
    for (const installer of manifest.Installers) {
      assert.deepEqual(installer.Dependencies.PackageDependencies, [
        { PackageIdentifier: `Microsoft.VCRedist.2015+.${installer.Architecture}` },
      ]);
    }
  }
});
