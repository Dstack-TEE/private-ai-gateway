import assert from "node:assert/strict";
import { execFile, spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import http from "node:http";
import { test } from "node:test";
import { parseEnv, promisify } from "node:util";

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

test("latest lookups retry transient failures and stop at the retry limit", async (t) => {
  for (const [name, statuses, succeeds] of [
    ["network error, rate limit and server error recover", [0, 429, 503, 200], true],
    ["persistent server errors fail after three retries", [503, 503, 503, 503], false],
    ["permanent client errors fail without retries", [404], false],
  ]) {
    await t.test(name, async () => {
      let attempts = 0;
      const server = http.createServer((request, response) => {
        const status = statuses[attempts++] ?? 500;
        if (status === 0) return request.socket.destroy();
        response.writeHead(status, { "content-type": "application/json" });
        response.end(JSON.stringify({ latest: "0.163.0" }));
      });
      await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
      const origin = `http://127.0.0.1:${server.address().port}`;
      // Keep native fetch and HTTP behavior, redirecting only its destination
      // into this local fixture rather than the public registry.
      const preload = `const fetch = globalThis.fetch; globalThis.fetch = (url, options) => fetch(${JSON.stringify(origin)}, options);`;
      try {
        const result = promisify(execFile)(process.execPath, [
          "--import", `data:text/javascript,${encodeURIComponent(preload)}`, script.pathname,
        ], {
          env: { ...process.env, ...pins, CODEX_VERSION: "latest" }, timeout: 15_000,
        });
        if (succeeds) {
          const { stdout } = await result;
          assert.equal(parseEnv(stdout).CODEX_VERSION, "0.163.0");
        } else {
          await assert.rejects(result, (error) => {
            assert.equal(error.code, 1);
            assert.equal(error.stdout, "");
            assert.match(error.stderr, /Resolving versions failed: .*HTTP (503|404)/);
            return true;
          });
        }
        assert.equal(attempts, statuses.length);
      } finally {
        await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
      }
    });
  }
});
