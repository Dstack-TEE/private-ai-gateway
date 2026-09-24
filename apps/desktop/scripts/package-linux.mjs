#!/usr/bin/env node
// Builds every Linux package, DEB, RPM and Arch Linux, with nfpm
// (https://nfpm.goreleaser.com), the packager behind GoReleaser. With
// `version_schema: semver` it writes a SemVer prerelease such as 1.2.3-beta.4
// as 1.2.3~beta.4 in DEB and RPM, which dpkg and rpm order before 1.2.3
// (Debian Policy 5.6.12, Fedora versioning guidelines). The desktop packages
// carry the payload Tauri bundled into its DEB; Tauri itself writes the SemVer
// string verbatim.
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readdir, rm, stat, writeFile } from "node:fs/promises";
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

const linuxPackages = {
  desktop: {
    name: "private-ai-proxy",
    description: "Private AI Proxy desktop application and command line client",
    other: "private-ai-proxy-cli",
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
    name: "private-ai-proxy-cli",
    description: "Private AI Proxy command line client and user backend",
    other: "private-ai-proxy",
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

// `contents` is the package's payload in nfpm terms; `marker` a file holding
// the owning package manager, when this format carries one.
function nfpmConfig(kind, packager, { version, arch, contents, marker }) {
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
    section: "utils",
    priority: "optional",
    maintainer: "Dstack <support@dstack.org>",
    homepage: "https://github.com/Dstack-TEE/private-ai-gateway",
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
    deb: { compression: "xz" },
    archlinux: { packager: "Dstack <support@dstack.org>" },
  };
}

// Writes `<output>/<artifact name>` in every Linux format and returns the paths.
export async function buildLinuxPackages({ kind, version, arch, contents, output }) {
  await mkdir(output, { recursive: true });
  const scratch = await mkdtemp(path.join(output, ".pap-nfpm-"));
  const packages = [];
  try {
    for (const [packager, { suffix, manager }] of Object.entries(packagers)) {
      let marker;
      if (linuxPackages[kind]?.markers.includes(packager)) {
        marker = path.join(scratch, `${packager}.marker`);
        await writeFile(marker, `${manager}\n`);
      }
      const config = path.join(scratch, `${packager}.json`);
      await writeFile(config, JSON.stringify(nfpmConfig(kind, packager, { version, arch, contents, marker })));
      const target = path.join(output, artifactName({ version, platform: "linux", arch, suffix, cli: kind === "cli" }));
      execFileSync(process.env.NFPM ?? "nfpm", ["package", "--config", config, "--packager", packager, "--target", target], { stdio: "inherit" });
      packages.push(target);
    }
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
  return packages;
}

// The payload's files, each with its mode. Like Tauri's own RPM, the package
// owns no directories: they are shared system paths. (nfpm's `type: tree`
// would own them and, in Arch packages, write Go's directory bit into their
// tar modes, which pacman reports as differing permissions.)
async function fileContents(root) {
  const contents = [];
  for (const entry of await readdir(root, { recursive: true, withFileTypes: true })) {
    if (entry.isDirectory()) continue;
    if (!entry.isFile()) throw new Error(`Unexpected payload entry ${entry.name}`);
    const file = path.join(entry.parentPath, entry.name);
    const { mode } = await stat(file);
    contents.push({ src: file, dst: `/${path.relative(root, file)}`, file_info: { mode: mode & 0o7777 } });
  }
  return contents;
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
    const contents = [...await fileContents(root), ...aliasLinks];
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
