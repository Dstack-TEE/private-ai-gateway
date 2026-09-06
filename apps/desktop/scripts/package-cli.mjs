#!/usr/bin/env node
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import {
  chmod,
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  readlink,
  rm,
  stat,
  symlink,
  writeFile,
} from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const binaries = ["pag", "pag-service", "aci", "private-ai-gateway-helper"];

const scriptPath = fileURLToPath(import.meta.url);
const appRoot = path.resolve(path.dirname(scriptPath), "..");

export function releaseVersionParts(version) {
  const match = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z.-]+))?$/.exec(version);
  if (!match) {
    throw new Error(`CLI package version must be SemVer, got ${JSON.stringify(version)}`);
  }
  const base = `${match[1]}.${match[2]}.${match[3]}`;
  const prerelease = match[4];
  return {
    deb: prerelease ? `${base}~${prerelease}` : base,
    rpmVersion: base,
    rpmRelease: prerelease ? `0.${prerelease.replace(/[^0-9A-Za-z.]+/g, ".")}.1` : "1",
  };
}

export async function stagePortable({ sourceDir, targetTriple, platform, destination }) {
  await mkdir(destination, { recursive: true });
  for (const name of binaries) {
    const extension = platform === "windows" ? ".exe" : "";
    const staged = path.join(sourceDir, `${name}-${targetTriple}${extension}`);
    const source = (await stat(staged).catch(() => undefined))?.isFile()
      ? staged : path.join(sourceDir, `${name}${extension}`);
    const target = path.join(destination, `${name}${extension}`);
    const metadata = await stat(source).catch(() => undefined);
    if (!metadata?.isFile()) {
      throw new Error(`Missing staged CLI binary ${source}`);
    }
    await copyFile(source, target);
    if (platform !== "windows") {
      await chmod(target, 0o755);
    }
  }
}

export async function stageLinuxPackageRoot(portableDirectory, packageRoot) {
  const libexec = path.join(packageRoot, "usr/libexec/private-ai-gateway");
  const bin = path.join(packageRoot, "usr/bin");
  await mkdir(libexec, { recursive: true });
  await mkdir(bin, { recursive: true });
  for (const name of binaries) {
    const target = path.join(libexec, name);
    await copyFile(path.join(portableDirectory, name), target);
    await chmod(target, 0o755);
  }
  await symlink("../libexec/private-ai-gateway/pag", path.join(bin, "pag"));
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  await mkdir(options.output, { recursive: true });
  const scratch = await mkdtemp(path.join(options.output, ".pag-cli-"));
  const artifactBase = `private-ai-gateway-cli-${options.version}-${options.platform}-${options.arch}`;
  const portable = path.join(scratch, artifactBase);
  const artifacts = [];

  try {
    await stagePortable({
      sourceDir: options.source,
      targetTriple: options.targetTriple,
      platform: options.platform,
      destination: portable,
    });
    const archive = path.join(
      options.output,
      `${artifactBase}.${options.platform === "windows" ? "zip" : "tar.gz"}`,
    );
    createArchive(options.platform, scratch, artifactBase, archive);
    artifacts.push(archive);

    if (options.platform === "linux") {
      const packageRoot = path.join(scratch, "package-root");
      await stageLinuxPackageRoot(portable, packageRoot);
      artifacts.push(await createDeb(options, scratch, packageRoot));
      artifacts.push(await createRpm(options, scratch, portable));
    }

    for (const artifact of artifacts) {
      await writeChecksum(artifact);
      console.log(`Packaged ${artifact}`);
    }
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
      throw new Error("Usage: package-cli.mjs --platform <windows|macos|linux> --arch <x64|arm64> --version <semver> --target-triple <triple> [--source <dir>] [--output <dir>]");
    }
    values.set(key.slice(2), value);
  }
  const platform = values.get("platform");
  const arch = values.get("arch");
  const version = values.get("version");
  const targetTriple = values.get("target-triple");
  if (!["windows", "macos", "linux"].includes(platform)) {
    throw new Error(`Unsupported CLI package platform ${JSON.stringify(platform)}`);
  }
  if (!["x64", "arm64"].includes(arch)) {
    throw new Error(`Unsupported CLI package architecture ${JSON.stringify(arch)}`);
  }
  if (!targetTriple || !/^[A-Za-z0-9_.-]+$/.test(targetTriple)) {
    throw new Error("A valid --target-triple is required");
  }
  releaseVersionParts(version);
  if ((platform === "windows") !== targetTriple.includes("windows")) {
    throw new Error("CLI package platform does not match the Rust target triple");
  }
  return {
    platform,
    arch,
    version,
    targetTriple,
    source: path.resolve(values.get("source") ?? path.join(appRoot, "src-tauri/binaries")),
    output: path.resolve(values.get("output") ?? path.join(appRoot, "release")),
  };
}

function createArchive(platform, parent, directory, output) {
  if (platform === "windows") {
    execFileSync("tar", ["-a", "-c", "-f", output, "-C", parent, directory], { stdio: "inherit" });
  } else {
    execFileSync("tar", ["-c", "-z", "-f", output, "-C", parent, directory], { stdio: "inherit" });
  }
}

async function createDeb(options, scratch, packageRoot) {
  const { deb } = releaseVersionParts(options.version);
  const architecture = options.arch === "x64" ? "amd64" : "arm64";
  const controlDir = path.join(packageRoot, "DEBIAN");
  await mkdir(controlDir, { recursive: true });
  const installedSize = Math.max(1, Math.ceil((await treeSize(packageRoot)) / 1024));
  await writeFile(
    path.join(controlDir, "control"),
    `Package: private-ai-gateway-cli\nVersion: ${deb}\nSection: utils\nPriority: optional\nArchitecture: ${architecture}\nInstalled-Size: ${installedSize}\nMaintainer: Dstack <support@dstack.org>\nHomepage: https://github.com/Dstack-TEE/private-ai-gateway\nDescription: Private AI Gateway command line client and user backend\n`,
  );
  await copyInstallerScript("deb-pre-install.sh", path.join(controlDir, "preinst"));
  await copyInstallerScript("deb-pre-remove.sh", path.join(controlDir, "prerm"));
  const output = path.join(options.output, `private-ai-gateway-cli_${deb}_${architecture}.deb`);
  execFileSync("dpkg-deb", ["--build", "--root-owner-group", packageRoot, output], { stdio: "inherit" });
  return output;
}

async function createRpm(options, scratch, portable) {
  const { rpmVersion, rpmRelease } = releaseVersionParts(options.version);
  const architecture = options.arch === "x64" ? "x86_64" : "aarch64";
  const topDir = path.join(scratch, "rpmbuild");
  const sources = path.join(topDir, "SOURCES");
  const specs = path.join(topDir, "SPECS");
  await mkdir(sources, { recursive: true });
  await mkdir(specs, { recursive: true });
  for (const name of binaries) {
    await copyFile(path.join(portable, name), path.join(sources, name));
  }
  const preInstall = await rpmScriptlet("rpm-pre-install.sh");
  const preRemove = await rpmScriptlet("rpm-pre-remove.sh");
  const spec = path.join(specs, "private-ai-gateway-cli.spec");
  await writeFile(
    spec,
    `Name: private-ai-gateway-cli\nVersion: ${rpmVersion}\nRelease: ${rpmRelease}\nSummary: Private AI Gateway command line client and user backend\nLicense: Apache-2.0\nURL: https://github.com/Dstack-TEE/private-ai-gateway\nBuildArch: ${architecture}\nAutoReqProv: no\n\n%description\nPrivate AI Gateway CLI, user-owned backend, verifier, and credential helper.\n\n%install\nrm -rf %{buildroot}\nmkdir -p %{buildroot}/usr/libexec/private-ai-gateway %{buildroot}/usr/bin\ninstall -m 0755 %{_sourcedir}/pag %{buildroot}/usr/libexec/private-ai-gateway/pag\ninstall -m 0755 %{_sourcedir}/pag-service %{buildroot}/usr/libexec/private-ai-gateway/pag-service\ninstall -m 0755 %{_sourcedir}/aci %{buildroot}/usr/libexec/private-ai-gateway/aci\ninstall -m 0755 %{_sourcedir}/private-ai-gateway-helper %{buildroot}/usr/libexec/private-ai-gateway/private-ai-gateway-helper\nln -s ../libexec/private-ai-gateway/pag %{buildroot}/usr/bin/pag\n\n%pre\n${preInstall}\n\n%preun\n${preRemove}\n\n%files\n/usr/bin/pag\n/usr/libexec/private-ai-gateway/aci\n/usr/libexec/private-ai-gateway/pag\n/usr/libexec/private-ai-gateway/pag-service\n/usr/libexec/private-ai-gateway/private-ai-gateway-helper\n`,
  );
  execFileSync("rpmbuild", ["-bb", "--define", `_topdir ${topDir}`, "--target", architecture, spec], {
    stdio: "inherit",
  });
  const rpm = (await walk(path.join(topDir, "RPMS"))).find((file) => file.endsWith(".rpm"));
  if (!rpm) {
    throw new Error("rpmbuild did not produce an RPM package");
  }
  const output = path.join(options.output, `private-ai-gateway-cli-${options.version}.${architecture}.rpm`);
  await copyFile(rpm, output);
  return output;
}

async function copyInstallerScript(name, destination) {
  await copyFile(path.join(appRoot, "src-tauri/installer", name), destination);
  await chmod(destination, 0o755);
}

async function rpmScriptlet(name) {
  const script = await readFile(path.join(appRoot, "src-tauri/installer", name), "utf8");
  return script.replace(/^#![^\n]*\n/, "")
    .replaceAll('"private-ai-gateway"', '"private-ai-gateway-cli"')
    .replaceAll("%", "%%").trimEnd();
}

async function treeSize(root) {
  let size = 0;
  for (const entry of await readdir(root, { withFileTypes: true })) {
    const file = path.join(root, entry.name);
    if (entry.isDirectory()) size += await treeSize(file);
    else if (entry.isFile()) size += (await stat(file)).size;
    else if (entry.isSymbolicLink()) size += (await readlink(file)).length;
  }
  return size;
}

async function walk(root) {
  const files = [];
  for (const entry of await readdir(root, { withFileTypes: true })) {
    const file = path.join(root, entry.name);
    if (entry.isDirectory()) files.push(...await walk(file));
    else files.push(file);
  }
  return files;
}

async function writeChecksum(file) {
  const digest = createHash("sha256").update(await readFile(file)).digest("hex");
  await writeFile(`${file}.sha256`, `${digest}  ${path.basename(file)}\n`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === scriptPath) {
  await main();
}
