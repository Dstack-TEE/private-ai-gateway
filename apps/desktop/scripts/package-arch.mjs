#!/usr/bin/env node
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { copyFile, mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { releaseVersionParts, writeChecksum, writePackageManagerMarker } from "./package-cli.mjs";
import { artifactName } from "./release-artifacts.mjs";

const packages = {
  desktop: {
    name: "private-ai-proxy",
    description: "Private AI Proxy desktop application and command line client",
    depends: [
      "cairo",
      "dbus",
      "desktop-file-utils",
      "gdk-pixbuf2",
      "glib2",
      "glibc",
      "gtk3",
      "hicolor-icon-theme",
      "libayatana-appindicator",
      "libgcc",
      "libsoup3",
      "pango",
      "webkit2gtk-4.1",
    ],
    provides: ["private-ai-proxy-cli"],
    conflicts: ["private-ai-proxy-cli"],
  },
  cli: {
    name: "private-ai-proxy-cli",
    description: "Private AI Proxy command line client and user backend",
    depends: ["glibc", "libgcc"],
    provides: [],
    conflicts: ["private-ai-proxy"],
  },
};

const scriptPath = fileURLToPath(import.meta.url);

export function archPackageMetadata(kind, version, arch) {
  const definition = packages[kind];
  if (!definition) throw new Error(`Unsupported Arch package kind ${JSON.stringify(kind)}`);
  if (!["x64", "arm64"].includes(arch)) {
    throw new Error(`Unsupported Arch package architecture ${JSON.stringify(arch)}`);
  }
  const versions = releaseVersionParts(version);
  return {
    ...definition,
    version: versions.arch,
    // Both source DEBs use the Debian `~` prerelease form (see normalize-linux-packages.mjs).
    sourceVersion: versions.deb,
    arch: arch === "x64" ? "x86_64" : "aarch64",
  };
}

export function archInstallScript(packageName) {
  return `check_private_ai_proxy_processes() {
  local cli directory binary executable process
  cli=/usr/bin/private-ai-proxy
  [[ -e "$cli" || -L "$cli" ]] || return 0
  directory=$(dirname "$(readlink -f "$cli")")
  for binary in private-ai-proxy-desktop private-ai-proxy-service private-ai-proxy private-ai-proxy-helper; do
    executable="$directory/$binary"
    [[ -e "$executable" ]] || continue
    for process in /proc/[0-9]*/exe; do
      [[ "$(readlink "$process" 2>/dev/null || true)" != "$executable" ]] || {
        echo "Private AI Proxy is still running. Close the desktop app and, as the signed-in user, run: $cli --yes service stop" >&2
        return 1
      }
    done
  done
}

check_private_ai_proxy_owner() {
  local cli owner
  for cli in /usr/bin/private-ai-proxy /usr/bin/pap /usr/bin/aci; do
    [[ -e "$cli" || -L "$cli" ]] || continue
    owner=$(pacman -Qqo "$cli" 2>/dev/null || true)
    if [[ -z "$owner" || "$owner" != "${packageName}" ]]; then
      echo "Refusing to replace unrelated $cli\${owner:+ owned by $owner}." >&2
      return 1
    fi
  done
  check_private_ai_proxy_processes
}

pre_install() { check_private_ai_proxy_owner; }
pre_upgrade() { check_private_ai_proxy_owner; }
pre_remove() { check_private_ai_proxy_processes; }
`;
}

export function archPkgbuild(metadata, payloadChecksum) {
  const values = (items) => `(${items.map(shellQuote).join(" ")})`;
  const provides = metadata.provides.map((name) => `${name}=${metadata.version}`);
  return `pkgname=${shellQuote(metadata.name)}
pkgver=${shellQuote(metadata.version)}
pkgrel=1
pkgdesc=${shellQuote(metadata.description)}
arch=${values([metadata.arch])}
url='https://github.com/Dstack-TEE/private-ai-gateway'
license=('Apache-2.0')
depends=${values(metadata.depends)}
provides=${values(provides)}
conflicts=${values(metadata.conflicts)}
options=('!strip' '!debug')
install=${shellQuote(`${metadata.name}.install`)}
source=('payload.tar')
sha256sums=(${shellQuote(payloadChecksum)})

package() {
  cp -a "$srcdir/usr" "$pkgdir"/
}
`;
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  const metadata = archPackageMetadata(options.kind, options.version, options.arch);
  const source = await debMetadata(options.source);
  if (source.name !== metadata.name || source.version !== metadata.sourceVersion || source.arch !== (options.arch === "x64" ? "amd64" : "arm64")) {
    throw new Error(`DEB metadata does not match the requested Arch package: ${JSON.stringify(source)}`);
  }

  await mkdir(options.output, { recursive: true });
  const scratch = await mkdtemp(path.join(options.output, ".pap-arch-"));
  try {
    const root = path.join(scratch, "root");
    const build = path.join(scratch, "build");
    await mkdir(root);
    await mkdir(build);
    execFileSync("dpkg-deb", ["-x", options.source, root], { stdio: "inherit" });
    // pacman owns the result: the desktop disables in-app updates and both kinds print pacman upgrade steps.
    await writePackageManagerMarker(root, "pacman");

    const payload = path.join(build, "payload.tar");
    execFileSync("tar", ["-c", "-f", payload, "-C", root, "."], { stdio: "inherit" });
    const checksum = createHash("sha256").update(await readFile(payload)).digest("hex");
    await writeFile(path.join(build, "PKGBUILD"), archPkgbuild(metadata, checksum));
    await writeFile(path.join(build, `${metadata.name}.install`), archInstallScript(metadata.name));
    const makepkgEnvironment = {
      ...process.env,
      PACKAGER: "Dstack <support@dstack.org>",
      PKGEXT: ".pkg.tar.zst",
    };
    execFileSync("makepkg", ["--nodeps", "--clean", "--cleanbuild", "--force", "--noconfirm"], {
      cwd: build,
      env: makepkgEnvironment,
      stdio: "inherit",
    });
    const packagePath = execFileSync("makepkg", ["--packagelist"], {
      cwd: build,
      env: makepkgEnvironment,
      encoding: "utf8",
    })
      .trim().split(/\r?\n/).filter(Boolean).at(-1);
    if (!packagePath || !(await stat(packagePath).catch(() => undefined))?.isFile()) {
      throw new Error("makepkg did not produce an Arch Linux package");
    }
    const output = path.join(options.output, artifactName({
      version: options.version,
      platform: "linux",
      arch: options.arch,
      suffix: ".pkg.tar.zst",
      cli: options.kind === "cli",
    }));
    await copyFile(packagePath, output);
    await writeChecksum(output);
    console.log(`Packaged ${output}`);
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
}

function parseArguments(arguments_) {
  const values = new Map();
  for (let index = 0; index < arguments_.length; index += 2) {
    const key = arguments_[index];
    const value = arguments_[index + 1];
    if (!key?.startsWith("--") || value === undefined) {
      throw new Error("Usage: package-arch.mjs --kind <desktop|cli> --source <deb> --version <semver> --arch <x64|arm64> [--output <dir>]");
    }
    values.set(key.slice(2), value);
  }
  const kind = values.get("kind");
  const version = values.get("version");
  const arch = values.get("arch");
  const source = path.resolve(values.get("source") ?? "");
  if (!packages[kind] || !version || !arch || !values.get("source")) {
    throw new Error("A valid --kind, --source, --version and --arch are required");
  }
  archPackageMetadata(kind, version, arch);
  return { kind, version, arch, source, output: path.resolve(values.get("output") ?? "release") };
}

async function debMetadata(file) {
  if (!(await stat(file).catch(() => undefined))?.isFile()) throw new Error(`Missing DEB package ${file}`);
  const field = (name) => execFileSync("dpkg-deb", ["-f", file, name], { encoding: "utf8" }).trim();
  const name = field("Package");
  const version = field("Version");
  const arch = field("Architecture");
  return { name, version, arch };
}

function shellQuote(value) {
  return `'${String(value).replaceAll("'", `'\"'\"'`)}'`;
}

if (process.argv[1] && path.resolve(process.argv[1]) === scriptPath) {
  await main();
}
