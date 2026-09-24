// One source owns build runners, public artifact names and Tauri updater keys.
export const desktopBuilds = [
  {
    os: "macos-26",
    platform: "macos",
    arch: "arm64",
    target: "aarch64-apple-darwin",
    bundle_dir: "aarch64-apple-darwin/release/bundle",
    bundles: "app,dmg",
    packages: [{ suffix: ".app.tar.gz", targets: ["darwin-aarch64"] }],
  },
  {
    os: "macos-26",
    platform: "macos",
    arch: "x64",
    target: "x86_64-apple-darwin",
    bundle_dir: "x86_64-apple-darwin/release/bundle",
    bundles: "app,dmg",
    packages: [{ suffix: ".app.tar.gz", targets: ["darwin-x86_64"] }],
  },
  {
    os: "windows-2025",
    platform: "windows",
    arch: "x64",
    target: "",
    bundle_dir: "release/bundle",
    bundles: "nsis",
    packages: [{ suffix: ".exe", targets: ["windows-x86_64"] }],
  },
  {
    os: "windows-11-arm",
    platform: "windows",
    arch: "arm64",
    target: "",
    bundle_dir: "release/bundle",
    bundles: "nsis",
    packages: [{ suffix: ".exe", targets: ["windows-aarch64"] }],
  },
  {
    os: "ubuntu-22.04",
    platform: "linux",
    arch: "x64",
    target: "",
    bundle_dir: "release/bundle",
    bundles: "deb",
    packages: [
      { suffix: ".deb", targets: ["linux-x86_64-deb"] },
      { suffix: ".rpm", targets: ["linux-x86_64-rpm"] },
    ],
  },
  {
    os: "ubuntu-22.04-arm",
    platform: "linux",
    arch: "arm64",
    target: "",
    bundle_dir: "release/bundle",
    bundles: "deb",
    packages: [
      { suffix: ".deb", targets: ["linux-aarch64-deb"] },
      { suffix: ".rpm", targets: ["linux-aarch64-rpm"] },
    ],
  },
];

export const desktopPackages = desktopBuilds.flatMap(({ platform, arch, packages }) =>
  packages.map((entry) => ({ platform, arch, ...entry })),
);

export const linuxNativePackageSuffixes = [".deb", ".rpm", ".pkg.tar.zst"];

export const desktopTargets = desktopPackages.flatMap((entry) => entry.targets);

export function desktopBuildId({ platform, arch }) {
  return `${platform}-${arch}`;
}

export function selectDesktopBuilds(value = "all") {
  const available = new Map(desktopBuilds.map((build) => [desktopBuildId(build), build]));
  const requested = value.trim();
  if (requested === "all") return desktopBuilds;
  const ids = requested.split(",").map((id) => id.trim()).filter(Boolean);
  if (ids.length === 0 || new Set(ids).size !== ids.length || ids.some((id) => !available.has(id))) {
    throw new Error(`Platforms must be all or a unique comma-separated list of: ${[...available.keys()].join(", ")}`);
  }
  return ids.map((id) => available.get(id));
}

export function manifestTargets(manifest) {
  const targets = Object.keys(manifest?.platforms ?? {});
  if (targets.length === 0) {
    throw new Error("Update manifest contains no desktop targets");
  }
  const known = new Set(desktopTargets);
  const unsupported = targets.filter((target) => !known.has(target));
  if (unsupported.length > 0) {
    throw new Error(`Update manifest contains unsupported desktop targets: ${unsupported.join(", ")}`);
  }
  return targets;
}

export function artifactName({ version, platform, arch, suffix = "", cli = false }) {
  return `private-ai-proxy${cli ? "-cli" : ""}-${version}-${platform}-${arch}${suffix}`;
}
