import { execFileSync } from "node:child_process";
import { parseArgs } from "node:util";
import { fileURLToPath } from "node:url";
import path from "node:path";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const { values } = parseArgs({ args, options: {
  target: { type: "string", short: "t" },
  debug: { type: "boolean", short: "d" },
  help: { type: "boolean", short: "h" },
  version: { type: "boolean", short: "V" },
}, strict: false, allowPositionals: true });
const buildTarget = values.target ?? (process.env.PAP_BUILD_TARGET?.trim() || undefined);
const env = { ...process.env, ...(buildTarget ? { PAP_BUILD_TARGET: buildTarget } : {}) };
const run = (script, arguments_ = []) => execFileSync(process.execPath, [script, ...arguments_], { cwd: appRoot, env, stdio: "inherit" });

if (!values.help && !values.version) {
  run("scripts/prepare-brand.mjs");
  run("scripts/bundle-sidecars.mjs", values.debug ? ["--debug"] : []);
}
run("node_modules/@tauri-apps/cli/tauri.js", [
  "build", "--config", "src-tauri/tauri.brand.conf.json", ...args,
  ...(!values.target && buildTarget ? ["--target", buildTarget] : []),
]);
