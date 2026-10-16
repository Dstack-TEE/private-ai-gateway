import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { exampleModels, localApiExample } from "../src/renderer/lib/local-api-example.ts";

// The arguments a POSIX shell passes to curl, read by running the example
// with printf in its place and without the heredoc body.
function curlArguments(example) {
  const command = example.slice(0, example.indexOf(" <<'JSON'")).replace(/^curl /, "printf '%s\\0' ");
  return execFileSync("sh", ["-c", command], { encoding: "utf8", env: { PATH: process.env.PATH, PAP_API_KEY: "sk-env" } }).split("\0").slice(0, -1);
}

const key = `sk-it's "$HOME" \`id\` \\ %\u2028`;
const model = `vendor/"model" \\ é`;
const decisions = { model, state: { ticket: "App crashes on login after update" }, questions: { bug: { type: "noul", instructions: "Is this a bug report?" } } };
const answers = { bug: { type: "noul", noul: 0.87 } };

test("the curl example passes the URL and the key to curl as single words", { skip: process.platform === "win32" }, () => {
  const key = `sk-it's "$HOME" \`id\` \\ %`;
  const example = localApiExample("chat", "curl", "http://[::1]:4190", "vendor/model", key);
  assert.deepEqual(curlArguments(example), [
    "--fail-with-body", "--max-time", "60", "--request", "POST", "http://[::1]:4190/v1/chat/completions",
    "--header", `Authorization: Bearer ${key}`,
    "--header", "Content-Type: application/json",
    "--data", "@-",
  ]);
  const payload = JSON.stringify({ model: "vendor/model", messages: [{ role: "user", content: "Hello" }] }, null, 2);
  assert.ok(example.endsWith(`<<'JSON'\n${payload}\nJSON`));
});

test("a typical curl example leaves the URL bare and single-quotes the headers", () => {
  const example = localApiExample("chat", "curl", "http://127.0.0.1:4190", "model", "sk-pap-AbC123_-xyz");
  assert.deepEqual(example.split("\n").slice(0, 3), [
    "curl --fail-with-body --max-time 60 --request POST http://127.0.0.1:4190/v1/chat/completions \\",
    "  --header 'Authorization: Bearer sk-pap-AbC123_-xyz' \\",
    "  --header 'Content-Type: application/json' \\",
  ]);
});

test("without a key, the curl example reads it from PAP_API_KEY", { skip: process.platform === "win32" }, () => {
  const example = localApiExample("chat", "curl", "http://127.0.0.1:4190", "model");
  assert.ok(curlArguments(example).includes("Authorization: Bearer sk-env"));
});

test("chat examples offer models with chat completions, decisions examples models that output decisions", () => {
  const models = [
    { id: "any", supportedEndpoints: undefined, outputModalities: ["text"] },
    { id: "chat", supportedEndpoints: ["/v1/chat/completions"], outputModalities: ["text"] },
    { id: "responses", supportedEndpoints: ["/v1/responses"], outputModalities: ["text"] },
    { id: "decisions", supportedEndpoints: [], outputModalities: ["decisions"] },
  ].map((entry) => ({ name: entry.id, inputModalities: ["text"], capabilities: [], ...entry }));
  assert.deepEqual(exampleModels("chat", models).map((entry) => entry.id), ["any", "chat"]);
  assert.deepEqual(exampleModels("decisions", models).map((entry) => entry.id), ["decisions"]);
  assert.deepEqual(exampleModels("decisions", models.slice(0, 3)), []);
});

test("the decisions curl example posts the System One body", { skip: process.platform === "win32" }, () => {
  const example = localApiExample("decisions", "curl", "http://127.0.0.1:4190", model, key);
  assert.deepEqual(curlArguments(example), [
    "--fail-with-body", "--max-time", "60", "--request", "POST", "http://127.0.0.1:4190/v1/systemone",
    "--header", `Authorization: Bearer ${key}`,
    "--header", "Content-Type: application/json",
    "--data", "@-",
  ]);
  assert.deepEqual(JSON.parse(example.slice(example.indexOf("<<'JSON'\n") + 9, -"\nJSON".length)), decisions);
});

test("the decisions JavaScript example asks the TypeSafe SDK and prints the answer", () => {
  const example = localApiExample("decisions", "javascript", "http://127.0.0.1:4190", model, key);
  // Runs the example against a stand-in for @typesafe-ai/sdk that records the calls.
  const directory = mkdtempSync(join(tmpdir(), "local-api-example-"));
  try {
    const sdk = join(directory, "node_modules", "@typesafe-ai", "sdk");
    mkdirSync(sdk, { recursive: true });
    writeFileSync(join(sdk, "package.json"), JSON.stringify({ name: "@typesafe-ai/sdk", type: "module", exports: "./index.js" }));
    writeFileSync(join(sdk, "index.js"), `
const calls = [];
process.on("exit", () => console.log(JSON.stringify(calls)));
export const noul = (instructions) => ({ type: "noul", instructions });
export class TypeSafeClient {
  constructor(config) { calls.push(config); }
  async systemOne(request) { calls.push(request); return ${JSON.stringify({ model, answers, usage: {} })}; }
}
`);
    writeFileSync(join(directory, "example.mjs"), example);
    const [answerLine, callsLine] = execFileSync(process.execPath, [join(directory, "example.mjs")], { encoding: "utf8" }).trim().split("\n");
    assert.equal(answerLine, "0.87");
    assert.deepEqual(JSON.parse(callsLine), [{ baseURL: "http://127.0.0.1:4190", apiKey: key }, decisions]);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("the decisions Python example asks the TypeSafe SDK and prints the answer", { skip: process.platform === "win32" }, () => {
  const example = localApiExample("decisions", "python", "http://127.0.0.1:4190", model, key);
  // Runs the example against a stand-in for typesafe_sdk that records the calls.
  const harness = `
import json, sys, types
calls = []
def Noul(**kwargs): return {"type": "noul", **kwargs}
class TypeSafeClient:
    def __init__(self, **kwargs): calls.append(kwargs)
    def __enter__(self): return self
    def __exit__(self, *args): calls.append("closed")
    def system_one(self, **kwargs):
        calls.append(kwargs)
        return types.SimpleNamespace(nouls={"bug": types.SimpleNamespace(noul=0.87)})
sys.modules["typesafe_sdk"] = types.SimpleNamespace(Noul=Noul, TypeSafeClient=TypeSafeClient)
exec(sys.argv[1])
print(json.dumps(calls))
`;
  const [answerLine, callsLine] = execFileSync("python3", ["-c", harness, example], { encoding: "utf8" }).trim().split("\n");
  assert.equal(answerLine, "0.87");
  assert.deepEqual(JSON.parse(callsLine), [{ api_key: key, base_url: "http://127.0.0.1:4190" }, decisions, "closed"]);
});
