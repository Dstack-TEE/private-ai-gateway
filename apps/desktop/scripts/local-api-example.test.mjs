import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
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

test("the decisions JavaScript example posts the System One body and prints the answers", async () => {
  const example = localApiExample("decisions", "javascript", "http://127.0.0.1:4190", model, key);
  const AsyncFunction = (async () => {}).constructor;
  const requests = [];
  const logged = [];
  const fetch = async (url, init) => {
    requests.push({ url, init });
    return { ok: true, json: async () => ({ model, answers, usage: {} }) };
  };
  await new AsyncFunction("fetch", "console", example)(fetch, { log: (value) => logged.push(value) });
  assert.equal(requests.length, 1);
  const [{ url, init }] = requests;
  assert.equal(url, "http://127.0.0.1:4190/v1/systemone");
  assert.equal(init.method, "POST");
  assert.deepEqual(init.headers, { Authorization: `Bearer ${key}`, "Content-Type": "application/json" });
  assert.deepEqual(JSON.parse(init.body), decisions);
  assert.ok(init.signal instanceof AbortSignal);
  assert.deepEqual(logged, [answers]);

  const failing = async () => ({ ok: false, status: 422, text: async () => "invalid questions" });
  await assert.rejects(new AsyncFunction("fetch", "console", example)(failing, console), /422 invalid questions/);
});

test("the decisions Python example posts the System One body and prints the answers", { skip: process.platform === "win32" }, () => {
  const example = localApiExample("decisions", "python", "http://127.0.0.1:4190", model, key);
  // Runs the example against a stand-in for requests that records the call.
  const harness = `
import json, sys, types
calls = []
class Response:
    def raise_for_status(self): calls.append("raise_for_status")
    def json(self): return json.loads(sys.argv[2])
def post(url, **kwargs):
    calls.append({"url": url, **kwargs})
    return Response()
sys.modules["requests"] = types.SimpleNamespace(post=post)
exec(sys.argv[1])
print(json.dumps(calls))
`;
  const [answersLine, callsLine] = execFileSync("python3", ["-c", harness, example, JSON.stringify({ model, answers, usage: {} })], { encoding: "utf8" }).trim().split("\n");
  assert.equal(answersLine, "{'bug': {'type': 'noul', 'noul': 0.87}}");
  assert.deepEqual(JSON.parse(callsLine), [
    { url: "http://127.0.0.1:4190/v1/systemone", headers: { Authorization: `Bearer ${key}` }, json: decisions, timeout: 60 },
    "raise_for_status",
  ]);
});
