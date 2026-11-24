import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { exampleModels, localApiExample } from "../src/renderer/lib/local-api-example.ts";

// The arguments a POSIX shell passes to curl, read by running the example
// with printf in its place and without the heredoc body, if any.
function curlArguments(example) {
  const heredoc = example.indexOf(" <<'JSON'");
  const command = example.slice(0, heredoc < 0 ? undefined : heredoc).replace(/^curl /, "printf '%s\\0' ");
  return execFileSync("sh", ["-c", command], { encoding: "utf8", env: { PATH: process.env.PATH, PAP_API_KEY: "sk-env" } }).split("\0").slice(0, -1);
}

const key = `sk-it's "$HOME" \`id\` \\ %\u2028`;
const model = `vendor/"model" \\ é`;
const decisions = { model, state: { ticket: "App crashes on login after update" }, questions: { bug: { type: "noul", instructions: "Is this a bug report?" } } };
const answers = { bug: { type: "noul", noul: 0.87 } };
const image = { model, prompt: "A watercolor lighthouse at dawn", n: 1, size: "512x512" };
const png = Buffer.concat([Buffer.from("89504e470d0a1a0a", "hex"), Buffer.from("PNG stand-in")]).toString("base64");
const jpeg = Buffer.concat([Buffer.from("ffd8ffe0", "hex"), Buffer.from("JPEG stand-in")]).toString("base64");

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

test("each example kind offers the models the Local API routes to its endpoint", () => {
  const models = [
    { id: "any", supportedEndpoints: undefined, outputModalities: ["text"] },
    { id: "chat", supportedEndpoints: ["/v1/chat/completions"], outputModalities: ["text"] },
    { id: "responses", supportedEndpoints: ["/v1/responses"], outputModalities: ["text"] },
    { id: "decisions", supportedEndpoints: [], outputModalities: ["decisions"] },
    { id: "image", supportedEndpoints: [], outputModalities: ["image"] },
    { id: "text-and-image", supportedEndpoints: undefined, outputModalities: ["text", "image"] },
    { id: "transcription", supportedEndpoints: [], inputModalities: ["audio"], outputModalities: ["transcription"] },
    { id: "audio-in", supportedEndpoints: undefined, inputModalities: ["text", "audio", "image"], outputModalities: ["text"] },
  ].map((entry) => ({ name: entry.id, inputModalities: ["text"], capabilities: [], ...entry }));
  assert.deepEqual(exampleModels("chat", models).map((entry) => entry.id), ["any", "chat", "text-and-image", "audio-in"]);
  assert.deepEqual(exampleModels("images", models).map((entry) => entry.id), ["image", "text-and-image"]);
  assert.deepEqual(exampleModels("transcription", models).map((entry) => entry.id), ["transcription"]);
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

test("the images curl example posts the generation body", { skip: process.platform === "win32" }, () => {
  const example = localApiExample("images", "curl", "http://127.0.0.1:4190", model, key);
  assert.deepEqual(curlArguments(example), [
    "--fail-with-body", "--max-time", "60", "--request", "POST", "http://127.0.0.1:4190/v1/images/generations",
    "--header", `Authorization: Bearer ${key}`,
    "--header", "Content-Type: application/json",
    "--output", "image.json",
    "--data", "@-",
  ]);
  assert.deepEqual(JSON.parse(example.slice(example.indexOf("<<'JSON'\n") + 9, -"\nJSON".length)), image);
});

test("the transcription curl example uploads the file and passes the model literally", { skip: process.platform === "win32" }, () => {
  const example = localApiExample("transcription", "curl", "http://127.0.0.1:4190", `${model};type=text/plain`, key);
  assert.deepEqual(curlArguments(example), [
    "--fail-with-body", "--max-time", "60", "--request", "POST", "http://127.0.0.1:4190/v1/audio/transcriptions",
    "--header", `Authorization: Bearer ${key}`,
    "--form", "file=@audio.wav",
    "--form-string", `model=${model};type=text/plain`,
    "--form-string", "response_format=json",
  ]);
  assert.ok(curlArguments(localApiExample("transcription", "curl", "http://127.0.0.1:4190", model)).includes("Authorization: Bearer sk-env"));
});

// Runs an OpenAI SDK example in a directory with audio.wav, against a stand-in
// for the SDK that records the calls and returns the base64 image given.
// Returns the output lines and the images the example saved.
function runOpenAiExample(example, run) {
  const directory = mkdtempSync(join(tmpdir(), "local-api-example-"));
  try {
    writeFileSync(join(directory, "audio.wav"), "RIFF stand-in");
    const output = run(directory).trim().split("\n");
    const saved = Object.fromEntries(readdirSync(directory).filter((file) => file.startsWith("image.")).map((file) => [file, readFileSync(join(directory, file)).toString("base64")]));
    return { output, saved };
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

function runJavaScript(example, b64 = png) {
  return runOpenAiExample(example, (directory) => {
    const sdk = join(directory, "node_modules", "openai");
    mkdirSync(sdk, { recursive: true });
    writeFileSync(join(sdk, "package.json"), JSON.stringify({ name: "openai", type: "module", exports: "./index.js" }));
    writeFileSync(join(sdk, "index.js"), `
const calls = [];
process.on("exit", () => console.log(JSON.stringify(calls)));
export default class OpenAI {
  constructor(config) { calls.push(config); }
  images = { generate: async (request) => { calls.push(request); return { data: [{ b64_json: ${JSON.stringify(b64)} }] }; } };
  audio = { transcriptions: { create: async ({ file, ...request }) => { calls.push({ ...request, file: file.path }); return { text: "Hello there" }; } } };
}
`);
    writeFileSync(join(directory, "example.mjs"), example);
    return execFileSync(process.execPath, ["example.mjs"], { cwd: directory, encoding: "utf8" });
  });
}

function runPython(example, b64 = png) {
  return runOpenAiExample(example, (directory) => {
    const harness = `
import json, sys, types
calls = []
class OpenAI:
    def __init__(self, **kwargs):
        calls.append(kwargs)
        self.images = types.SimpleNamespace(generate=self.generate)
        self.audio = types.SimpleNamespace(transcriptions=types.SimpleNamespace(create=self.transcribe))
    def generate(self, **kwargs):
        calls.append(kwargs)
        return types.SimpleNamespace(data=[types.SimpleNamespace(b64_json=${JSON.stringify(b64)})])
    def transcribe(self, file, **kwargs):
        calls.append({**kwargs, "file": file.name, "content": file.read().decode()})
        return types.SimpleNamespace(text="Hello there")
sys.modules["openai"] = types.SimpleNamespace(OpenAI=OpenAI)
exec(sys.argv[1])
print(json.dumps(calls))
`;
    return execFileSync("python3", ["-c", harness, example], { cwd: directory, encoding: "utf8" });
  });
}

for (const [language, run, call] of [
  ["JavaScript", runJavaScript, { baseURL: "http://127.0.0.1:4190/v1", apiKey: key }],
  ["Python", runPython, { api_key: key, base_url: "http://127.0.0.1:4190/v1" }],
]) {
  test(`the images ${language} example saves the image under the extension of its bytes`, { skip: language === "Python" && process.platform === "win32" }, () => {
    const example = localApiExample("images", language.toLowerCase(), "http://127.0.0.1:4190", model, key);
    for (const [b64, file] of [[png, "image.png"], [jpeg, "image.jpg"]]) {
      const { output, saved } = run(example, b64);
      assert.equal(output[0], `Saved ${file}`);
      assert.deepEqual(JSON.parse(output[1]), [call, image]);
      assert.deepEqual(saved, { [file]: b64 });
    }
  });
}

test("the transcription JavaScript example uploads audio.wav and prints the text", () => {
  const { output } = runJavaScript(localApiExample("transcription", "javascript", "http://127.0.0.1:4190", model, key));
  assert.equal(output[0], "Hello there");
  assert.deepEqual(JSON.parse(output[1]), [{ baseURL: "http://127.0.0.1:4190/v1", apiKey: key }, { model, response_format: "json", file: "audio.wav" }]);
});

test("the transcription Python example uploads audio.wav and prints the text", { skip: process.platform === "win32" }, () => {
  const { output } = runPython(localApiExample("transcription", "python", "http://127.0.0.1:4190", model, key));
  assert.equal(output[0], "Hello there");
  assert.deepEqual(JSON.parse(output[1]), [{ api_key: key, base_url: "http://127.0.0.1:4190/v1" }, { model, response_format: "json", file: "audio.wav", content: "RIFF stand-in" }]);
});
