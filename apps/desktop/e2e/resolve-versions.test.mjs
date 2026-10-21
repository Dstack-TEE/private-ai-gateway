import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { parseEnv } from "node:util";

const script = new URL("resolve-versions.mjs", import.meta.url);
const defaults = parseEnv(readFileSync(new URL("versions.env", import.meta.url), "utf8"));
const pins = Object.fromEntries(Object.keys(defaults).map((name) =>
  [name, name === "HERMES_VERSION" ? "v2026.9.24" : "1.2.3"]));

test("exact environment pins bypass latest resolution, including prereleases and Hermes tags", () => {
  const expected = { ...pins, DSH_VERSION: "0.2.1-rc.2", CODEX_VERSION: "0.157.1" };
  const result = spawnSync(process.execPath, [script.pathname], {
    env: { ...process.env, ...expected }, encoding: "utf8", timeout: 5_000,
  });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual({ ...parseEnv(result.stdout) }, expected);
});

test("rejects a non-release override without emitting a partial version file", () => {
  const result = spawnSync(process.execPath, [script.pathname], {
    env: { ...process.env, ...pins, CODEX_VERSION: "0.163.0\nNODE_VERSION=26.12.0" },
    encoding: "utf8", timeout: 5_000,
  });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Invalid CODEX_VERSION/);
  assert.equal(result.stdout, "");
});
