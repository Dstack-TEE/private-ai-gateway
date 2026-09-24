#!/usr/bin/env node
// Builds every Linux package, DEB, RPM and Arch Linux, with nfpm
// (https://nfpm.goreleaser.com), the packager behind GoReleaser. With
// `version_schema: semver` it writes a SemVer prerelease such as 1.2.3-beta.4
// as 1.2.3~beta.4 in DEB and RPM, which dpkg and rpm order before 1.2.3
// (Debian Policy 5.6.12, Fedora versioning guidelines). The desktop packages
// carry the payload Tauri bundled into its DEB; Tauri itself writes the SemVer
// string verbatim. Names and descriptions come from tauri.conf.json, as Tauri's do.
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import semver from "semver";

import { artifactName } from "./release-artifacts.mjs";

const scriptPath = fileURLToPath(import.meta.url);
const appRoot = path.resolve(path.dirname(scriptPath), "..");

// Each nfpm packager's file suffix and the package manager name the marker records.
const packagers = {
  deb: { suffix: ".deb", manager: "deb" },
  rpm: { suffix: ".rpm", manager: "rpm" },
  archlinux: { suffix: ".pkg.tar.zst", manager: "pacman" },
};
// Names the package manager that owns an install, so update notices can print
// its upgrade command (core/src/updates.rs). The desktop needs it only under
// pacman, where it disables in-app installation.
const markerDirectory = "/usr/share/private-ai-proxy";
// `pap` and the legacy `aci` are symlinks, so the executable sees the alias
// in argv[0].
export const aliases = ["pap", "aci"];
export const aliasLinks = aliases.map((alias) => ({ src: "private-ai-proxy", dst: `/usr/bin/${alias}`, type: "symlink" }));
// Debian Policy 6.6: lets an upgrade continue past the refusing prerm of
// packages up to 0.1.7-beta.n. Remove in 0.3.
const debPrerm = path.join(appRoot, "src-tauri/installer/deb-prerm.sh");

// Tauri names the desktop package after the product and describes it with the
// bundle metadata; the CLI and Arch packages use the same values.
const tauri = JSON.parse(await readFile(path.join(appRoot, "src-tauri/tauri.conf.json"), "utf8"));
const desktopName = tauri.productName.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
const cliName = `${desktopName}-cli`;
// Debian Policy 5.6.2: the maintainer is a name and an email address.
export const maintainer = `${tauri.bundle.publisher} <support@phala.com>`;

const linuxPackages = {
  desktop: {
    name: desktopName,
    description: `${tauri.bundle.shortDescription}\n${tauri.bundle.longDescription}`,
    other: cliName,
    // The desktop package includes the CLI, so it stands in for it.
    provides: true,
    markers: ["archlinux"],
    // The libraries Tauri's bundler declares for WebKitGTK, GTK and the
    // Ayatana tray (tauri-cli src/interface/rust.rs), in each format's terms.
    depends: {
      deb: ["libayatana-appindicator3-1", "libwebkit2gtk-4.1-0", "libgtk-3-0"],
      rpm: ["libayatana-appindicator3.so.1()(64bit)", "libwebkit2gtk-4.1.so.0()(64bit)", "libgtk-3.so.0()(64bit)"],
      archlinux: [
        "cairo", "dbus", "desktop-file-utils", "gdk-pixbuf2", "glib2", "glibc", "gtk3", "hicolor-icon-theme",
        "libayatana-appindicator", "libgcc", "libsoup3", "pango", "webkit2gtk-4.1",
      ],
    },
  },
  cli: {
    name: cliName,
    description: `${tauri.productName} command line client and user backend`,
    other: desktopName,
    provides: false,
    markers: ["deb", "rpm", "archlinux"],
    depends: { deb: [], rpm: [], archlinux: ["glibc", "libgcc"] },
  },
};

export function releaseVersionParts(version) {
  const parsed = semver.parse(version);
  if (!parsed || semver.valid(version) !== version || parsed.build.length > 0) {
    throw new Error(`Package version must be SemVer, got ${JSON.stringify(version)}`);
  }
  const base = `${parsed.major}.${parsed.minor}.${parsed.patch}`;
  const prerelease = parsed.prerelease.length > 0 ? parsed.prerelease.join(".") : undefined;
  return {
    // pacman sorts a trailing letter segment before the release: 1.2.3beta.4 < 1.2.3.
    arch: prerelease ? `${base}${prerelease.replace(/[^0-9A-Za-z.]+/g, ".")}` : base,
    // nfpm's forms (deb/deb.go and rpm/rpm.go formatVersion); rpm adds release 1.
    deb: prerelease ? `${base}~${prerelease}` : base,
    rpm: `${prerelease ? `${base}~${prerelease.replaceAll("-", "_")}` : base}-1`,
  };
}

// nfpm otherwise stamps entries with the time it writes each one, so an Arch
// package's .MTREE and tar headers disagree and `pacman -Qkk` reports altered
// files. Like GoReleaser's `mtime: "{{ .CommitDate }}"`, every entry gets
// SOURCE_DATE_EPOCH (reproducible-builds.org) or the commit time, which also
// makes the packages reproducible.
function packageTime() {
  const epoch = process.env.SOURCE_DATE_EPOCH?.trim()
    || execFileSync("git", ["log", "-1", "--format=%ct"], { cwd: appRoot, encoding: "utf8" }).trim();
  const seconds = Number(epoch);
  if (!Number.isSafeInteger(seconds) || seconds < 0) throw new Error(`Invalid package time ${JSON.stringify(epoch)}`);
  return new Date(seconds * 1000).toISOString();
}

// `contents` is the package's payload in nfpm terms; `marker` a file holding
// the owning package manager, when this format carries one.
function nfpmConfig(kind, packager, { version, arch, contents, marker, mtime }) {
  const definition = linuxPackages[kind];
  if (!definition) throw new Error(`Unsupported Linux package kind ${JSON.stringify(kind)}`);
  if (!packagers[packager]) throw new Error(`Unsupported Linux packager ${JSON.stringify(packager)}`);
  if (!["x64", "arm64"].includes(arch)) throw new Error(`Unsupported Linux package architecture ${JSON.stringify(arch)}`);
  const versions = releaseVersionParts(version);
  // nfpm's archlinux packager drops a SemVer prerelease from pkgver
  // (arch/arch.go), so it gets the native pacman version instead.
  const pacman = packager === "archlinux";
  // Debian Policy 7.6.2: Conflicts and Replaces with the other package, plus
  // Provides from the desktop. RPM and pacman get no Obsoletes/replaces, which
  // would swap packages on every system upgrade.
  const provides = {
    deb: `${definition.other} (= ${versions.deb})`,
    rpm: `${definition.other} = ${versions.rpm}`,
    archlinux: `${definition.other}=${versions.arch}`,
  }[packager];
  return {
    name: definition.name,
    arch: arch === "x64" ? "amd64" : "arm64",
    platform: "linux",
    version: pacman ? versions.arch : version,
    version_schema: pacman ? "none" : "semver",
    mtime,
    section: "utils",
    priority: "optional",
    maintainer,
    homepage: tauri.bundle.homepage,
    license: "Apache-2.0",
    description: definition.description,
    depends: definition.depends[packager],
    provides: definition.provides ? [provides] : [],
    conflicts: [definition.other],
    replaces: packager === "deb" ? [definition.other] : [],
    contents: [
      ...contents,
      ...(marker ? [
        { dst: markerDirectory, type: "dir" },
        { src: marker, dst: `${markerDirectory}/package-manager`, file_info: { mode: 0o644 } },
      ] : []),
    ],
    overrides: { deb: { scripts: { preremove: debPrerm } } },
    deb: { compression: "xz" },
    archlinux: { packager: maintainer },
  };
}

// Writes `<output>/<artifact name>` in every Linux format and returns the paths.
export async function buildLinuxPackages({ kind, version, arch, contents, output }) {
  await mkdir(output, { recursive: true });
  const scratch = await mkdtemp(path.join(output, ".pap-nfpm-"));
  const packages = [];
  const mtime = packageTime();
  try {
    for (const [packager, { suffix, manager }] of Object.entries(packagers)) {
      let marker;
      if (linuxPackages[kind]?.markers.includes(packager)) {
        marker = path.join(scratch, `${packager}.marker`);
        await writeFile(marker, `${manager}\n`);
      }
      const config = path.join(scratch, `${packager}.json`);
      await writeFile(config, JSON.stringify(nfpmConfig(kind, packager, { version, arch, contents, marker, mtime })));
      const target = path.join(output, artifactName({ version, platform: "linux", arch, suffix, cli: kind === "cli" }));
      execFileSync(process.env.NFPM ?? "nfpm", ["package", "--config", config, "--packager", packager, "--target", target], { stdio: "inherit" });
      packages.push(target);
    }
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
  return packages;
}

// Repackages the Tauri DEB in `bundleDir/deb` in every Linux format under
// `output`. When Tauri signed its DEB for the updater, the DEB and RPM the
// updater installs are signed, so every signature covers the bytes that ship.
export async function packageDesktop({ bundleDir, version, arch, output }) {
  const debDirectory = path.join(bundleDir, "deb");
  const built = (await readdir(debDirectory)).filter((name) => name.endsWith(".deb"));
  if (built.length !== 1) throw new Error(`Expected one Tauri DEB in ${debDirectory}; found ${built.length}`);
  const tauriDeb = path.join(debDirectory, built[0]);
  const name = execFileSync("dpkg-deb", ["-f", tauriDeb, "Package"], { encoding: "utf8" }).trim();
  if (name !== linuxPackages.desktop.name) throw new Error(`Tauri built package ${JSON.stringify(name)}, expected ${linuxPackages.desktop.name}`);
  const signed = Boolean(await stat(`${tauriDeb}.sig`).catch(() => undefined));
  if (signed && !process.env.TAURI_SIGNING_PRIVATE_KEY && !process.env.TAURI_SIGNING_PRIVATE_KEY_PATH) {
    throw new Error("Cannot sign the desktop packages without the updater signing key");
  }
  const scratch = await mkdtemp(path.join(bundleDir, ".pap-linux-"));
  let packages;
  try {
    const root = path.join(scratch, "root");
    execFileSync("dpkg-deb", ["-x", tauriDeb, root], { stdio: "inherit" });
    const contents = [{ src: path.join(root, "usr"), dst: "/usr" }, ...aliasLinks];
    packages = await buildLinuxPackages({ kind: "desktop", version, arch, contents, output });
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
  if (signed) {
    for (const file of packages.filter((file) => file.endsWith(".deb") || file.endsWith(".rpm"))) {
      execFileSync(process.execPath, [
        path.join(appRoot, "node_modules/@tauri-apps/cli/tauri.js"), "signer", "sign", "--app-version", version, file,
      ], { stdio: "inherit" });
      if (!(await stat(`${file}.sig`).catch(() => undefined))?.isFile()) throw new Error(`Missing signature for ${file}`);
    }
  }
  return packages;
}

if (process.argv[1] && path.resolve(process.argv[1]) === scriptPath) {
  const [bundleDir, version, arch, output] = process.argv.slice(2);
  if (!bundleDir || !version || !arch || !output) {
    throw new Error("Usage: package-linux.mjs <Tauri bundle directory> <version> <x64|arm64> <output directory>");
  }
  const packages = await packageDesktop({ bundleDir: path.resolve(bundleDir), version, arch, output: path.resolve(output) });
  for (const file of packages) console.log(`Packaged ${file}`);
}
