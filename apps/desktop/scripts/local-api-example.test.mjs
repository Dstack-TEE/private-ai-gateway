import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { test } from "node:test";
import { localApiExample } from "../src/renderer/lib/local-api-example.ts";

// The arguments a POSIX shell passes to curl, read by running the example
// with printf in its place and without the heredoc body.
function curlArguments(example) {
  const command = example.slice(0, example.indexOf(" <<'JSON'")).replace(/^curl /, "printf '%s\\0' ");
  return execFileSync("sh", ["-c", command], { encoding: "utf8", env: { PATH: process.env.PATH, PAP_API_KEY: "sk-env" } }).split("\0").slice(0, -1);
}

test("the curl example passes the URL and the key to curl as single words", { skip: process.platform === "win32" }, () => {
  const key = `sk-it's "$HOME" \`id\` \\ %`;
  const example = localApiExample("curl", "http://[::1]:4190", "vendor/model", key);
  assert.deepEqual(curlArguments(example), [
    "--fail-with-body", "--max-time", "60", "--request", "POST", "http://[::1]:4190/v1/chat/completions",
    "--header", `Authorization: Bearer ${key}`,
    "--header", "Content-Type: application/json",
    "--data", "@-",
  ]);
  const payload = JSON.stringify({ model: "vendor/model", messages: [{ role: "user", content: "Hello" }] }, null, 2);
  assert.ok(example.endsWith(`<<'JSON'\n${payload}\nJSON`));
});

test("without a key, the curl example reads it from PAP_API_KEY", { skip: process.platform === "win32" }, () => {
  const example = localApiExample("curl", "http://127.0.0.1:4190", "model");
  assert.ok(curlArguments(example).includes("Authorization: Bearer sk-env"));
});
