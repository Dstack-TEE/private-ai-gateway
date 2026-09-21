import { execFileSync } from "node:child_process";
import { parseArgs } from "node:util";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { MAC_APP_STORE_DISTRIBUTION, runtimeBuildVersion, takeDistributionArgument } from "./distribution.mjs";
import { UNIVERSAL_MACOS_TARGET } from "./build-config.mjs";

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
if (appStore && (process.env.TAURI_UPDATER_PUBLIC_KEY?.trim() || process.env.TAURI_UPDATER_ENDPOINT?.trim())) {
  throw new Error("Mac App Store builds cannot include the native updater");
}
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
  run("scripts/prepare-brand.mjs");
  run("scripts/bundle-sidecars.mjs", values.debug ? ["--debug"] : []);
}
run("node_modules/@tauri-apps/cli/tauri.js", [
  "build", "--config", "src-tauri/tauri.brand.conf.json",
  ...(appStore ? ["--config", "src-tauri/tauri.appstore.conf.json"] : []),
  ...(appStore ? ["--no-sign"] : []),
  ...args,
  ...(!values.target && buildTarget ? ["--target", buildTarget] : []),
]);
