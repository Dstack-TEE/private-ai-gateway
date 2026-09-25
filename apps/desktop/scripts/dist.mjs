import { execFileSync } from "node:child_process";
import { parseArgs } from "node:util";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { MAC_APP_STORE_DISTRIBUTION, runtimeBuildVersion, takeDistributionArgument } from "./distribution.mjs";
import { UNIVERSAL_MACOS_TARGET } from "./build-config.mjs";
import { releaseConfig } from "./release-config.mjs";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const selected = takeDistributionArgument(process.argv.slice(2));
const args = selected.args;
const { values } = parseArgs({ args, options: {
  target: { type: "string", short: "t" },
  debug: { type: "boolean", short: "d" },
  help: { type: "boolean", short: "h" },
  version: { type: "boolean", short: "V" },
}, strict: false, allowPositionals: true });
const appStore = selected.distribution === MAC_APP_STORE_DISTRIBUTION;
const requestedTarget = values.target ?? (process.env.PAP_BUILD_TARGET?.trim() || undefined);
if (appStore && process.platform !== "darwin") {
  throw new Error("Mac App Store builds require macOS and Xcode");
}
if (appStore && requestedTarget && requestedTarget !== UNIVERSAL_MACOS_TARGET) {
  throw new Error(`Mac App Store builds use ${UNIVERSAL_MACOS_TARGET}`);
}
const release = releaseConfig(selected.distribution);
const buildTarget = appStore ? UNIVERSAL_MACOS_TARGET : requestedTarget;
const buildVersion = runtimeBuildVersion();
const env = {
  ...process.env,
  PAP_DISTRIBUTION: selected.distribution,
  ...(buildVersion ? { PAP_BUILD_VERSION: buildVersion } : {}),
  ...(buildTarget ? { PAP_BUILD_TARGET: buildTarget } : {}),
};
const run = (script, arguments_ = []) => execFileSync(process.execPath, [script, ...arguments_], { cwd: appRoot, env, stdio: "inherit" });

if (!values.help && !values.version) {
  run("scripts/bundle-sidecars.mjs", values.debug ? ["--debug"] : []);
}
run("node_modules/@tauri-apps/cli/tauri.js", [
  "build",
  ...(appStore ? ["--config", "src-tauri/tauri.appstore.conf.json"] : []),
  ...(Object.keys(release).length ? ["--config", JSON.stringify(release)] : []),
  ...(appStore ? ["--no-sign"] : []),
  ...args,
  ...(!values.target && buildTarget ? ["--target", buildTarget] : []),
]);
