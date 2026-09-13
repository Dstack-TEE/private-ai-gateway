const all = [
  { os: "macos-26", platform: "macos-arm64", cli_platform: "macos", cli_arch: "arm64", target: "aarch64-apple-darwin", bundle_dir: "aarch64-apple-darwin/release/bundle", bundles: "app,dmg" },
  { os: "macos-26", platform: "macos-x64", cli_platform: "macos", cli_arch: "x64", target: "x86_64-apple-darwin", bundle_dir: "x86_64-apple-darwin/release/bundle", bundles: "app,dmg" },
  { os: "windows-2025", platform: "windows-x64", cli_platform: "windows", cli_arch: "x64", target: "", bundle_dir: "release/bundle", bundles: "nsis" },
  { os: "windows-11-arm", platform: "windows-arm64", cli_platform: "windows", cli_arch: "arm64", target: "", bundle_dir: "release/bundle", bundles: "nsis" },
  { os: "ubuntu-24.04", platform: "linux-x64", cli_platform: "linux", cli_arch: "x64", target: "", bundle_dir: "release/bundle", bundles: "deb,rpm" },
  { os: "ubuntu-24.04-arm", platform: "linux-arm64", cli_platform: "linux", cli_arch: "arm64", target: "", bundle_dir: "release/bundle", bundles: "deb,rpm" },
];
const value = process.argv[2]?.trim() || "all";
const selected = value === "all" ? all : value.split(",").map((item) => item.trim()).filter(Boolean);
const valid = new Set(all.map((item) => item.platform));
if (value !== "all" && (selected.some((item) => !valid.has(item)) || new Set(selected).size !== selected.length)) {
  throw new Error(`Platforms must be all or a unique comma-separated list of: ${[...valid].join(", ")}`);
}
const matrix = value === "all" ? all : all.filter((item) => selected.includes(item.platform));
if (matrix.length === 0) throw new Error("At least one package platform is required");
process.stdout.write(JSON.stringify({ include: matrix }));
