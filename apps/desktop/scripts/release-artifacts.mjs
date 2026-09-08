// Public names use macos/windows/linux and universal/x64/arm64.
// Updater keys retain the target names required by Tauri.
export const desktopPackages = [
  { platform: "macos", arch: "arm64", suffix: ".app.tar.gz", targets: ["darwin-aarch64"] },
  { platform: "macos", arch: "x64", suffix: ".app.tar.gz", targets: ["darwin-x86_64"] },
  { platform: "windows", arch: "x64", suffix: ".exe", targets: ["windows-x86_64"] },
  { platform: "linux", arch: "x64", suffix: ".deb", targets: ["linux-x86_64-deb"] },
  { platform: "linux", arch: "x64", suffix: ".rpm", targets: ["linux-x86_64-rpm"] },
];

export function artifactName({ version, platform, arch, suffix = "", cli = false }) {
  return `private-ai-proxy${cli ? "-cli" : ""}-${version}-${platform}-${arch}${suffix}`;
}
