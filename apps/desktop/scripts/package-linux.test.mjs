import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { binaries, linuxContents } from "./package-cli.mjs";
import { buildLinuxPackages, maintainer, packageDesktop, releaseVersionParts } from "./package-linux.mjs";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
// A fixed package time (2026-01-01) instead of the commit time.
process.env.SOURCE_DATE_EPOCH = "1767225600";
const available = (tool) => {
  try {
    execFileSync("sh", ["-c", `command -v ${tool}`], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
};
const output = (command, args) => execFileSync(command, args, { encoding: "utf8" }).trim();
const rpmCompare = (left, right) => Number(output("rpm", ["--eval", `%{lua: print(rpm.vercmp("${left}", "${right}"))}`]));

test("maps SemVer to the native versions nfpm writes", () => {
  assert.deepEqual(releaseVersionParts("1.2.3-beta.4"), { arch: "1.2.3beta.4", deb: "1.2.3~beta.4", rpm: "1.2.3~beta.4-1" });
  assert.deepEqual(releaseVersionParts("1.2.3"), { arch: "1.2.3", deb: "1.2.3", rpm: "1.2.3-1" });
  assert.throws(() => releaseVersionParts("1.2"), /must be SemVer/);
});

// Installed releases up to 0.1.7-beta.n used these versions; each must upgrade.
const upgrades = [
  { deb: "0.1.7~beta.4", rpm: "0.1.7-0.beta.4.1" },
  { deb: "0.1.7-beta.1", rpm: "0.1.7-beta.1-1" },
  { deb: "0.1.6", rpm: "0.1.6-1" },
];

test("dpkg orders prereleases before their release and after earlier packages", { skip: !available("dpkg") }, () => {
  const beta = releaseVersionParts("0.2.0-beta.1").deb;
  const stable = releaseVersionParts("0.2.0").deb;
  for (const { deb } of upgrades) execFileSync("dpkg", ["--compare-versions", deb, "lt", beta]);
  execFileSync("dpkg", ["--compare-versions", beta, "lt", releaseVersionParts("0.2.0-beta.2").deb]);
  execFileSync("dpkg", ["--compare-versions", beta, "lt", stable]);
});

test("rpm orders prereleases before their release and after earlier packages", { skip: !available("rpm") }, () => {
  const beta = releaseVersionParts("0.2.0-beta.1").rpm;
  const stable = releaseVersionParts("0.2.0").rpm;
  for (const { rpm } of upgrades) assert.equal(rpmCompare(rpm, beta), -1, `${rpm} < ${beta}`);
  assert.equal(rpmCompare(beta, releaseVersionParts("0.2.0-beta.2").rpm), -1);
  assert.equal(rpmCompare(beta, stable), -1);
});

const nfpmAvailable = available("nfpm") && available("dpkg-deb") && available("bsdtar");
// libarchive reads RPM and Arch packages alike.
const member = (file, name) => execFileSync("bsdtar", ["-xOf", file, name], { encoding: "utf8" });
const pkginfo = (file) => member(file, ".PKGINFO");
const debListing = (file) => output("dpkg-deb", ["-c", file]);

test("the Tauri payload ships as DEB, RPM and Arch packages with signed updater packages", { skip: !nfpmAvailable }, async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "pap-package-linux-"));
  try {
    const payload = path.join(root, "payload");
    await mkdir(path.join(payload, "DEBIAN"), { recursive: true });
    await mkdir(path.join(payload, "usr/bin"), { recursive: true });
    await mkdir(path.join(payload, "usr/share/applications"), { recursive: true });
    await writeFile(path.join(payload, "usr/bin/private-ai-proxy"), "binary", { mode: 0o755 });
    await writeFile(path.join(payload, "usr/share/applications/Private AI Proxy.desktop"), "entry");
    await writeFile(path.join(payload, "DEBIAN/control"), "Package: private-ai-proxy\nVersion: 1.2.3-beta.4\nArchitecture: amd64\nMaintainer: Test\nDescription: Test\n");
    const bundle = path.join(root, "bundle");
    await mkdir(path.join(bundle, "deb"), { recursive: true });
    const tauriDeb = path.join(bundle, "deb/Private AI Proxy_1.2.3-beta.4_amd64.deb");
    execFileSync("dpkg-deb", ["--build", "--root-owner-group", payload, tauriDeb], { stdio: "ignore" });
    await writeFile(`${tauriDeb}.sig`, "stale");
    execFileSync(process.execPath, [path.join(appRoot, "node_modules/@tauri-apps/cli/tauri.js"), "signer", "generate", "--ci", "-p", "", "-w", path.join(root, "key")], { stdio: "ignore" });

    const environment = { ...process.env };
    const release = path.join(root, "release");
    let packages;
    try {
      process.env.TAURI_SIGNING_PRIVATE_KEY = await readFile(path.join(root, "key"), "utf8");
      process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD = "";
      packages = await packageDesktop({ bundleDir: bundle, version: "1.2.3-beta.4", arch: "x64", output: release });
    } finally {
      process.env = environment;
    }

    const [deb, rpm, arch] = [".deb", ".rpm", ".pkg.tar.zst"].map((suffix) => path.join(release, `private-ai-proxy-1.2.3-beta.4-linux-x64${suffix}`));
    assert.deepEqual(packages, [deb, rpm, arch]);
    assert.deepEqual((await readdir(release)).sort(), [deb, `${deb}.sig`, arch, rpm, `${rpm}.sig`].map((file) => path.basename(file)).sort());
    assert.equal(output("dpkg-deb", ["-f", deb, "Version"]), "1.2.3~beta.4");
    assert.equal(output("dpkg-deb", ["-f", deb, "Depends"]), "libayatana-appindicator3-1, libwebkit2gtk-4.1-0, libgtk-3-0");
    assert.equal(output("dpkg-deb", ["-f", deb, "Provides"]), "private-ai-proxy-cli (= 1.2.3~beta.4)");
    assert.equal(output("dpkg-deb", ["-f", deb, "Conflicts"]), "private-ai-proxy-cli");
    assert.equal(output("dpkg-deb", ["-f", deb, "Replaces"]), "private-ai-proxy-cli");
    assert.match(debListing(deb), /-rwxr-xr-x root\/root +6 .* \.\/usr\/bin\/private-ai-proxy$/m);
    assert.match(debListing(deb), /\.\/usr\/bin\/pap -> private-ai-proxy$/m);
    assert.match(debListing(deb), /\.\/usr\/bin\/aci -> private-ai-proxy$/m);
    assert.match(debListing(deb), /\.\/usr\/share\/applications\/Private AI Proxy\.desktop$/m);
    assert.doesNotMatch(debListing(deb), /package-manager/);
    const tauri = JSON.parse(await readFile(path.join(appRoot, "src-tauri/tauri.conf.json"), "utf8"));
    // Debian Policy 5.6.2: a name and an email address, in every format.
    assert.equal(output("dpkg-deb", ["-f", deb, "Maintainer"]), maintainer);
    assert.equal(output("dpkg-deb", ["-f", deb, "Homepage"]), tauri.bundle.homepage);
    assert.equal(output("dpkg-deb", ["-f", deb, "Description"]), `${tauri.bundle.shortDescription}\n ${tauri.bundle.longDescription}`);
    // Every entry carries SOURCE_DATE_EPOCH (2026-01-01), so builds are reproducible.
    assert.doesNotMatch(debListing(deb), /^(?!.* 2026-01-01 00:00 ).+$/m);
    // The only maintainer script is the prerm that lets 0.1.x upgrades continue (Debian Policy 6.6).
    assert.match(output("dpkg-deb", ["-I", deb]), /^ +\d+ bytes, +\d+ lines +\* +prerm +#!\/bin\/sh$/m);
    assert.doesNotMatch(output("dpkg-deb", ["-I", deb]), /preinst|postinst|postrm/);
    execFileSync("sh", ["-c", 'dpkg-deb -I "$1" prerm | sh -s failed-upgrade 0.1.7~beta.4', "sh", deb]);
    if (available("rpm")) {
      const query = (format) => output("rpm", ["-qp", "--qf", format, rpm]);
      assert.equal(query("%{NAME} %{EPOCH}:%{VERSION}-%{RELEASE}"), "private-ai-proxy (none):1.2.3~beta.4-1");
      assert.equal(query("%{PACKAGER}"), maintainer);
      assert.equal(
        query("[%{FILENAMES}|%{FILEMODES:perms}|%{FILELINKTOS}\n]").replace(/\|l[-rwx]{9}\|/g, "|link|"),
        "/usr/bin/aci|link|private-ai-proxy\n/usr/bin/pap|link|private-ai-proxy\n/usr/bin/private-ai-proxy|-rwxr-xr-x|\n/usr/share/applications/Private AI Proxy.desktop|-rw-r--r--|",
      );
      assert.match(output("rpm", ["-qp", "--requires", rpm]), /^libwebkit2gtk-4\.1\.so\.0\(\)\(64bit\)$/m);
      assert.match(output("rpm", ["-qp", "--provides", rpm]), /^private-ai-proxy-cli = 1\.2\.3~beta\.4-1$/m);
      assert.equal(output("rpm", ["-qp", "--conflicts", rpm]), "private-ai-proxy-cli");
      assert.equal(output("rpm", ["-qp", "--obsoletes", rpm]), "");
      assert.equal(output("rpm", ["-qp", "--scripts", rpm]), "");
    }
    const info = pkginfo(arch);
    assert.match(info, new RegExp(`^packager = ${maintainer}$`, "m"));
    assert.match(info, /^pkgver = 1\.2\.3beta\.4-1$/m);
    assert.match(info, /^provides = private-ai-proxy-cli=1\.2\.3beta\.4$/m);
    assert.match(info, /^conflict = private-ai-proxy-cli$/m);
    assert.match(info, /^depend = webkit2gtk-4\.1$/m);
    assert.doesNotMatch(info, /^replaces/m);
    assert.equal(member(arch, "usr/share/private-ai-proxy/package-manager"), "pacman\n");
    assert.doesNotMatch(execFileSync("bsdtar", ["-tf", arch], { encoding: "utf8" }), /\.INSTALL/);
    for (const file of [deb, rpm]) {
      const signature = Buffer.from(await readFile(`${file}.sig`, "utf8"), "base64").toString("utf8");
      assert.match(signature, new RegExp(`trusted comment: timestamp:\\d+\\tfile:${path.basename(file).replaceAll(".", "\\.")}\\tversion:1\\.2\\.3-beta\\.4`));
    }
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("CLI packages keep executables in libexec, alias symlinks and their package manager", { skip: !nfpmAvailable }, async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "pap-package-cli-linux-"));
  try {
    const portable = path.join(root, "portable");
    await mkdir(portable);
    for (const name of binaries) await writeFile(path.join(portable, name), name, { mode: 0o755 });
    const release = path.join(root, "release");
    const [deb, rpm, arch] = await buildLinuxPackages({ kind: "cli", version: "1.2.3", arch: "arm64", contents: linuxContents(portable), output: release });
    assert.equal(path.basename(deb), "private-ai-proxy-cli-1.2.3-linux-arm64.deb");
    assert.equal(output("dpkg-deb", ["-f", deb, "Version"]), "1.2.3");
    assert.equal(output("dpkg-deb", ["-f", deb, "Conflicts"]), "private-ai-proxy");
    assert.equal(output("dpkg-deb", ["-f", deb, "Replaces"]), "private-ai-proxy");
    assert.equal(output("dpkg-deb", ["-f", deb, "Provides"]), "");
    const listing = debListing(deb);
    assert.match(listing, /\.\/usr\/bin\/private-ai-proxy -> \.\.\/libexec\/private-ai-proxy\/private-ai-proxy$/m);
    assert.match(listing, /\.\/usr\/bin\/pap -> private-ai-proxy$/m);
    assert.match(listing, /\.\/usr\/bin\/aci -> private-ai-proxy$/m);
    for (const name of binaries) assert.match(listing, new RegExp(`-rwxr-xr-x root/root .* \\./usr/libexec/private-ai-proxy/${name}$`, "m"));
    // An optional user unit that nothing enables.
    assert.match(listing, /-rw-r--r-- root\/root .* \.\/usr\/lib\/systemd\/user\/private-ai-proxy\.service$/m);
    const extracted = path.join(root, "deb");
    execFileSync("dpkg-deb", ["-x", deb, extracted]);
    assert.equal(await readFile(path.join(extracted, "usr/share/private-ai-proxy/package-manager"), "utf8"), "deb\n");
    if (available("rpm")) {
      assert.equal(output("rpm", ["-qp", "--qf", "%{VERSION}-%{RELEASE} %{ARCH}", rpm]), "1.2.3-1 aarch64");
      const files = output("rpm", ["-qp", "--qf", "[%{FILENAMES}|%{FILEMODES:perms}\n]", rpm]);
      assert.match(files, /^\/usr\/libexec\/private-ai-proxy\|drwxr-xr-x$/m);
      assert.match(files, /^\/usr\/share\/private-ai-proxy\|drwxr-xr-x$/m);
      assert.equal(member(rpm, "/usr/share/private-ai-proxy/package-manager"), "rpm\n");
    }
    assert.match(pkginfo(arch), /^depend = glibc$/m);
    assert.match(pkginfo(arch), /^arch = aarch64$/m);
    assert.equal(member(arch, "usr/share/private-ai-proxy/package-manager"), "pacman\n");
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
