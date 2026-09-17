import { selectDesktopBuilds } from "./release-artifacts.mjs";

const selected = selectDesktopBuilds(process.argv[2]?.trim() || "all");
const matrix = selected.map(({ platform, arch, packages: _packages, ...build }) => ({
  ...build,
  platform: `${platform}-${arch}`,
  cli_platform: platform,
  cli_arch: arch,
}));
process.stdout.write(JSON.stringify({ include: matrix }));
