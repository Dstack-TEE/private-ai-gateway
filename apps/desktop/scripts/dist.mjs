import { execFileSync } from "node:child_process";
import { parseArgs } from "node:util";
import { fileURLToPath } from "node:url";
import path from "node:path";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const prepareOnly = args.includes("--prepare-only");
const bundleOnly = args.includes("--bundle-only");
if (prepareOnly && bundleOnly) throw new Error("Choose either --prepare-only or --bundle-only");
const tauriArgs = args.filter((argument) => argument !== "--prepare-only" && argument !== "--bundle-only");
const { values } = parseArgs({ args, options: {
  target: { type: "string", short: "t" },
  debug: { type: "boolean", short: "d" },
  help: { type: "boolean", short: "h" },
  version: { type: "boolean", short: "V" },
}, strict: false, allowPositionals: true });
const buildTarget = values.target ?? (process.env.PAP_BUILD_TARGET?.trim() || undefined);
const env = { ...process.env, ...(buildTarget ? { PAP_BUILD_TARGET: buildTarget } : {}) };
const run = (script, arguments_ = []) => execFileSync(process.execPath, [script, ...arguments_], { cwd: appRoot, env, stdio: "inherit" });

if (!bundleOnly && !values.help && !values.version) {
  run("scripts/prepare-brand.mjs");
  run("scripts/bundle-sidecars.mjs", values.debug ? ["--debug"] : []);
}
if (!prepareOnly) {
  run("node_modules/@tauri-apps/cli/tauri.js", [
    "build", "--config", "src-tauri/tauri.brand.conf.json", ...tauriArgs,
    ...(!values.target && buildTarget ? ["--target", buildTarget] : []),
  ]);
}
