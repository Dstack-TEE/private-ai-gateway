#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readdir, readFile, rename, rm, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { releaseVersionParts } from "./package-cli.mjs";

const scriptPath = fileURLToPath(import.meta.url);
const appRoot = path.resolve(path.dirname(scriptPath), "..");

// Tauri writes the SemVer string verbatim into DEB and RPM metadata, where
// 0.1.7-beta.1 sorts after 0.1.7 and blocks the stable upgrade. Rewrite the
// desktop packages to the native prerelease forms used by the CLI packages.
// A rewritten package gets a fresh updater signature over its final bytes.
export async function normalizeLinuxPackages(bundleDir, version) {
  const rewritten = [];
  for (const [kind, rewrite] of [["deb", rewriteDeb], ["rpm", rewriteRpm]]) {
    const directory = path.join(bundleDir, kind);
    if (!(await stat(directory).catch(() => undefined))?.isDirectory()) continue;
    for (const name of await readdir(directory)) {
      if (!name.endsWith(`.${kind}`)) continue;
      const file = path.join(directory, name);
      if (await rewrite(file, version)) rewritten.push(file);
    }
  }
  for (const file of rewritten) {
    const signature = `${file}.sig`;
    if (!(await stat(signature).catch(() => undefined))) continue;
    await rm(signature);
    if (!process.env.TAURI_SIGNING_PRIVATE_KEY && !process.env.TAURI_SIGNING_PRIVATE_KEY_PATH) {
      throw new Error(`Cannot re-sign ${path.basename(file)} without the updater signing key`);
    }
    execFileSync(process.execPath, [
      path.join(appRoot, "node_modules/@tauri-apps/cli/tauri.js"), "signer", "sign", "--app-version", version, file,
    ], { stdio: "inherit" });
    if (!(await stat(signature).catch(() => undefined))?.isFile()) throw new Error(`Missing signature for ${file}`);
  }
  return rewritten;
}

export async function rewriteDeb(file, version) {
  const { deb } = releaseVersionParts(version);
  const field = (name) => execFileSync("dpkg-deb", ["-f", file, name], { encoding: "utf8" }).trim();
  if (field("Version") === deb) return false;
  const scratch = await mkdtemp(path.join(path.dirname(file), ".pap-deb-"));
  try {
    const root = path.join(scratch, "root");
    execFileSync("dpkg-deb", ["-R", file, root], { stdio: "inherit" });
    const controlFile = path.join(root, "DEBIAN/control");
    const control = await readFile(controlFile, "utf8");
    if ((control.match(/^Version: .*$/gm) ?? []).length !== 1) throw new Error(`Unexpected DEB control file in ${file}`);
    await writeFile(controlFile, control.replace(/^Version: .*$/m, `Version: ${deb}`));
    const output = path.join(scratch, "package.deb");
    execFileSync("dpkg-deb", ["--build", "--root-owner-group", root, output], { stdio: "inherit" });
    await rename(output, file);
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
  return true;
}

export async function rewriteRpm(file, version) {
  const { rpmVersion, rpmRelease } = releaseVersionParts(version);
  const query = (format) => execFileSync("rpm", ["-qp", "--qf", format, file], { encoding: "utf8" });
  if (query("%{VERSION}") === rpmVersion && query("%{RELEASE}") === rpmRelease) return false;
  const scratch = await mkdtemp(path.join(path.dirname(file), ".pap-rpm-"));
  try {
    const root = path.join(scratch, "root");
    const topDir = path.join(scratch, "rpmbuild");
    await mkdir(root);
    execFileSync("sh", ["-c", 'rpm2cpio "$1" | cpio -idm --quiet --no-absolute-filenames', "sh", file], { cwd: root });
    const spec = path.join(scratch, "package.spec");
    await writeFile(spec, rpmSpec({
      name: query("%{NAME}"),
      version: rpmVersion,
      release: rpmRelease,
      summary: query("%{SUMMARY}"),
      license: query("%{LICENSE}"),
      url: query("%{URL}"),
      description: query("%{DESCRIPTION}"),
      requires: execFileSync("rpm", ["-qp", "--requires", file], { encoding: "utf8" }).split("\n"),
      scriptlets: { pre: query("%{PREIN}"), preun: query("%{PREUN}"), post: query("%{POSTIN}"), postun: query("%{POSTUN}") },
      files: query("[%{FILENAMES}\n]").split("\n"),
      root,
    }));
    const arch = query("%{ARCH}");
    execFileSync("rpmbuild", ["-bb", "--quiet", "--define", `_topdir ${topDir}`, "--target", arch, spec], { stdio: "inherit" });
    const rpms = path.join(topDir, "RPMS", arch);
    const [built, ...extra] = (await readdir(rpms)).filter((name) => name.endsWith(".rpm"));
    if (!built || extra.length) throw new Error(`rpmbuild did not produce exactly one package for ${file}`);
    await rename(path.join(rpms, built), file);
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
  return true;
}

// Repackage the unchanged payload, dependencies and scriptlets under a new
// version. Desktop RPMs carry no epoch, matching the CLI RPM.
export function rpmSpec({ name, version, release, summary, license, url, description, requires, scriptlets, files, root }) {
  const escape = (value) => value.replaceAll("%", "%%");
  const present = (value) => value && value !== "(none)";
  const lines = [
    "%global __os_install_post %{nil}",
    "%global debug_package %{nil}",
    "%define _build_id_links none",
    `Name: ${name}`,
    `Version: ${version}`,
    `Release: ${release}`,
    `Summary: ${escape(summary)}`,
    `License: ${escape(license)}`,
    ...(present(url) ? [`URL: ${escape(url)}`] : []),
    "AutoReqProv: no",
    ...requires.map((line) => line.trim()).filter((line) => line && !line.startsWith("rpmlib(")).map((line) => `Requires: ${line}`),
    "",
    "%description",
    escape(description),
    "",
    "%install",
    `cp -a ${JSON.stringify(escape(root))}/. %{buildroot}/`,
  ];
  for (const [section, body] of Object.entries(scriptlets)) {
    if (present(body)) lines.push("", `%${section}`, escape(body.replace(/^#![^\n]*\n/, "").trimEnd()));
  }
  lines.push("", "%files", "%defattr(-,root,root,-)", ...files.filter(Boolean).map((file) => JSON.stringify(escape(file))), "");
  return lines.join("\n");
}

if (process.argv[1] && path.resolve(process.argv[1]) === scriptPath) {
  const [bundleDir, version] = process.argv.slice(2);
  if (!bundleDir || !version) throw new Error("Usage: normalize-linux-packages.mjs <bundle directory> <version>");
  releaseVersionParts(version);
  for (const file of await normalizeLinuxPackages(path.resolve(bundleDir), version)) console.log(`Normalized ${file}`);
}
