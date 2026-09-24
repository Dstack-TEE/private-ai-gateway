// Public artifact names and the Tauri updater key each signed desktop package
// serves. The build matrix is in .github/workflows/desktop-native.yml.
export const desktopPackages = [
  { platform: "macos", arch: "arm64", suffix: ".app.tar.gz", target: "darwin-aarch64" },
  { platform: "macos", arch: "x64", suffix: ".app.tar.gz", target: "darwin-x86_64" },
  { platform: "windows", arch: "x64", suffix: ".exe", target: "windows-x86_64" },
  { platform: "windows", arch: "arm64", suffix: ".exe", target: "windows-aarch64" },
  { platform: "linux", arch: "x64", suffix: ".deb", target: "linux-x86_64-deb" },
  { platform: "linux", arch: "x64", suffix: ".rpm", target: "linux-x86_64-rpm" },
  { platform: "linux", arch: "arm64", suffix: ".deb", target: "linux-aarch64-deb" },
  { platform: "linux", arch: "arm64", suffix: ".rpm", target: "linux-aarch64-rpm" },
];

export const desktopTargets = desktopPackages.map((entry) => entry.target);

export function manifestTargets(manifest) {
  const targets = Object.keys(manifest?.platforms ?? {});
  if (targets.length === 0) {
    throw new Error("Update manifest contains no desktop targets");
  }
  const unsupported = targets.filter((target) => !desktopTargets.includes(target));
  if (unsupported.length > 0) {
    throw new Error(`Update manifest contains unsupported desktop targets: ${unsupported.join(", ")}`);
  }
  return targets;
}

export function artifactName({ version, platform, arch, suffix = "", cli = false }) {
  return `private-ai-proxy${cli ? "-cli" : ""}-${version}-${platform}-${arch}${suffix}`;
}
