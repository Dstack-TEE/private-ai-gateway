import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { normalizeLinuxPackages } from "./normalize-linux-packages.mjs";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const available = (tool) => {
  try {
    execFileSync("sh", ["-c", `command -v ${tool}`], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
};
const output = (command, args) => execFileSync(command, args, { encoding: "utf8" }).trim();

async function withScratch(prefix, run) {
  const root = await mkdtemp(path.join(os.tmpdir(), prefix));
  try {
    await run(root);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

test("desktop DEB prereleases sort before the stable release and are re-signed", { skip: !available("dpkg-deb") }, async () => {
  await withScratch("pap-normalize-deb-", async (root) => {
    const packageRoot = path.join(root, "package");
    await mkdir(path.join(packageRoot, "DEBIAN"), { recursive: true });
    await mkdir(path.join(packageRoot, "usr/bin"), { recursive: true });
    await writeFile(path.join(packageRoot, "usr/bin/private-ai-proxy"), "binary");
    await writeFile(path.join(packageRoot, "DEBIAN/control"), "Package: private-ai-proxy\nVersion: 1.2.3-beta.4\nArchitecture: amd64\nMaintainer: Test\nDescription: Test\n");
    await writeFile(path.join(packageRoot, "DEBIAN/preinst"), "#!/bin/sh\nexit 0\n", { mode: 0o755 });
    const bundle = path.join(root, "bundle");
    await mkdir(path.join(bundle, "deb"), { recursive: true });
    const deb = path.join(bundle, "deb/Private AI Proxy_1.2.3-beta.4_amd64.deb");
    execFileSync("dpkg-deb", ["--build", "--root-owner-group", packageRoot, deb], { stdio: "ignore" });
    await writeFile(`${deb}.sig`, "stale");
    execFileSync(process.execPath, [path.join(appRoot, "node_modules/@tauri-apps/cli/tauri.js"), "signer", "generate", "--ci", "-p", "", "-w", path.join(root, "key")], { stdio: "ignore" });

    const environment = { ...process.env };
    try {
      process.env.TAURI_SIGNING_PRIVATE_KEY = await readFile(path.join(root, "key"), "utf8");
      process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD = "";
      assert.deepEqual(await normalizeLinuxPackages(bundle, "1.2.3-beta.4"), [deb]);
    } finally {
      process.env = environment;
    }

    assert.equal(output("dpkg-deb", ["-f", deb, "Version"]), "1.2.3~beta.4");
    assert.match(output("dpkg-deb", ["-c", deb]), /root\/root.*\.\/usr\/bin\/private-ai-proxy/);
    execFileSync("dpkg", ["--compare-versions", "1.2.3~beta.4", "lt", "1.2.3"]);
    const signature = Buffer.from(await readFile(`${deb}.sig`, "utf8"), "base64").toString("utf8");
    assert.match(signature, /trusted comment: timestamp:\d+\tfile:Private AI Proxy_1\.2\.3-beta\.4_amd64\.deb\tversion:1\.2\.3-beta\.4/);
    assert.deepEqual(await normalizeLinuxPackages(bundle, "1.2.3-beta.4"), [], "normalization is idempotent");
  });
});

test("desktop RPM prereleases keep payload, dependencies and scriptlets under native versions", { skip: !available("rpmbuild") || !available("rpm2cpio") || !available("cpio") }, async () => {
  await withScratch("pap-normalize-rpm-", async (root) => {
    const spec = path.join(root, "fixture.spec");
    await writeFile(spec, [
      "%global __os_install_post %{nil}",
      "%global debug_package %{nil}",
      "Name: private-ai-proxy", "Version: 1.2.3", "Release: 1", "Summary: Fixture", "License: MIT", "AutoReqProv: no",
      "Requires: libgtk-3.so.0()(64bit)",
      "%description", "Fixture with 100%% coverage", "%install",
      'mkdir -p %{buildroot}/usr/bin "%{buildroot}/usr/share/applications"',
      "printf binary > %{buildroot}/usr/bin/private-ai-proxy && chmod 0755 %{buildroot}/usr/bin/private-ai-proxy",
      'printf entry > "%{buildroot}/usr/share/applications/Private AI Proxy.desktop"',
      "%pre", 'echo "pre $1"', "%preun", 'echo "preun $1"',
      "%files", "/usr/bin/private-ai-proxy", '"/usr/share/applications/Private AI Proxy.desktop"', "",
    ].join("\n"));
    execFileSync("rpmbuild", ["-bb", "--quiet", "--define", `_topdir ${path.join(root, "fixture")}`, "--target", "x86_64", spec], { stdio: "ignore" });
    const bundle = path.join(root, "bundle");
    await mkdir(path.join(bundle, "rpm"), { recursive: true });
    const rpm = path.join(bundle, "rpm/Private AI Proxy-1.2.3-beta.4-1.x86_64.rpm");
    execFileSync("cp", [path.join(root, "fixture/RPMS/x86_64/private-ai-proxy-1.2.3-1.x86_64.rpm"), rpm]);

    assert.deepEqual(await normalizeLinuxPackages(bundle, "1.2.3-beta.4"), [rpm]);

    const query = (format) => output("rpm", ["-qp", "--qf", format, rpm]);
    assert.equal(query("%{EPOCH}:%{VERSION}-%{RELEASE}"), "(none):1.2.3-0.beta.4.1");
    assert.equal(query("%{DESCRIPTION}"), "Fixture with 100% coverage");
    assert.equal(query("[%{FILENAMES}|%{FILEMODES:perms}\n]"), "/usr/bin/private-ai-proxy|-rwxr-xr-x\n/usr/share/applications/Private AI Proxy.desktop|-rw-r--r--");
    assert.match(output("rpm", ["-qp", "--requires", rpm]), /^libgtk-3\.so\.0\(\)\(64bit\)$/m);
    assert.match(output("rpm", ["-qp", "--scripts", rpm]), /echo "pre \$1"[\s\S]*echo "preun \$1"/);
    assert.equal(execFileSync("sh", ["-c", 'rpm2cpio "$1" | cpio -i --quiet --to-stdout ./usr/bin/private-ai-proxy', "sh", rpm], { encoding: "utf8" }), "binary");
    assert.deepEqual(await normalizeLinuxPackages(bundle, "1.2.3-beta.4"), [], "normalization is idempotent");
  });
});
