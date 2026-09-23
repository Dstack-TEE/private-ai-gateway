#!/usr/bin/env node
// Writes src/shared/contracts.generated.ts from the Rust contracts through the
// runtime test that also fails CI when the committed file is stale. Pass
// `--check` to only verify it.
import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const check = process.argv.includes("--check");
const result = spawnSync(
  process.env.CARGO ?? "cargo",
  [
    "test",
    "--locked",
    "--package",
    "private-ai-proxy-runtime",
    "--lib",
    "contracts::typescript",
  ],
  {
    cwd: appRoot,
    stdio: "inherit",
    env: check ? process.env : { ...process.env, PAP_WRITE_CONTRACTS: "1" },
  },
);
if (result.error) throw result.error;
process.exit(result.status ?? 1);
