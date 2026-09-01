import { cp, mkdir, rm } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { build } from "esbuild";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const dist = path.join(appRoot, "dist");

await rm(dist, { force: true, recursive: true });
await mkdir(path.join(dist, "renderer"), { recursive: true });

await Promise.all([
  build({
    bundle: true,
    entryPoints: [path.join(appRoot, "src/main/index.ts")],
    external: ["electron"],
    format: "cjs",
    outfile: path.join(dist, "main/index.cjs"),
    platform: "node",
    sourcemap: true,
    target: "node22",
  }),
  build({
    bundle: true,
    entryPoints: [path.join(appRoot, "src/preload/index.ts")],
    external: ["electron"],
    format: "cjs",
    outfile: path.join(dist, "preload/index.cjs"),
    platform: "node",
    sourcemap: true,
    target: "node22",
  }),
  build({
    bundle: true,
    entryPoints: [path.join(appRoot, "src/renderer/index.tsx")],
    format: "iife",
    loader: { ".tsx": "tsx" },
    outfile: path.join(dist, "renderer/app.js"),
    platform: "browser",
    sourcemap: true,
    target: ["chrome136"],
  }),
]);

await cp(
  path.join(appRoot, "src/renderer/index.html"),
  path.join(dist, "renderer/index.html"),
);
