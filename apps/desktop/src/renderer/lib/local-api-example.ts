import { join } from "shlex";
import type { ModelSummary } from "../../shared/contracts";

export type ExampleKind = "chat" | "images" | "transcription" | "decisions";
export type ExampleLanguage = "curl" | "python" | "javascript";

const paths: Record<ExampleKind, string> = {
  chat: "/v1/chat/completions",
  images: "/v1/images/generations",
  transcription: "/v1/audio/transcriptions",
  decisions: "/v1/systemone",
};

// Media models need an explicit output modality, as the Local API routes them.
const outputs = (modality: string) => (model: ModelSummary) => model.outputModalities.includes(modality);
const accepts: Record<ExampleKind, (model: ModelSummary) => boolean> = {
  chat: (model) => model.supportedEndpoints?.includes(paths.chat) ?? true,
  images: outputs("image"),
  transcription: outputs("transcription"),
  decisions: outputs("decisions"),
};

/** The catalog models an example of this kind can call. */
export function exampleModels(kind: ExampleKind, models: ModelSummary[]): ModelSummary[] {
  return models.filter(accepts[kind]);
}

const imagePrompt = "A watercolor lighthouse at dawn";
const audioFile = "audio.wav";
const imageFile = "image.png";

export function localApiExample(kind: ExampleKind, language: ExampleLanguage, endpoint: string, model: string, apiKey?: string): string {
  const url = new URL(paths[kind], endpoint);
  if (url.protocol !== "http:" && url.protocol !== "https:") throw new Error("Invalid Local API endpoint");
  const body = kind === "chat"
    ? { model, messages: [{ role: "user", content: "Hello" }] }
    : kind === "images"
      ? { model, prompt: imagePrompt, n: 1, size: "512x512" }
      : { model, state: { ticket: "App crashes on login after update" }, questions: { bug: { type: "noul", instructions: "Is this a bug report?" } } };
  const payload = JSON.stringify(body, null, 2);
  if (language === "curl") {
    return [
      `${join(["curl", "--fail-with-body", "--max-time", "60", "--request", "POST", url.toString()])} \\`,
      // Without a key, the shell supplies it from PAP_API_KEY or stops with the hint.
      apiKey ? `  ${join(["--header", `Authorization: Bearer ${apiKey}`])} \\` : '  --header "Authorization: Bearer ${PAP_API_KEY:?Set PAP_API_KEY to your local client key}" \\',
      ...kind === "transcription"
        // --form-string keeps the model id literal; curl would read ;type= or @ in --form.
        ? [`  ${join(["--form", `file=@${audioFile}`])} \\`, `  ${join(["--form-string", `model=${model}`])} \\`, `  ${join(["--form-string", "response_format=json"])}`]
        : [`  ${join(["--header", "Content-Type: application/json"])} \\`, "  --data @- <<'JSON'", payload, "JSON"],
    ].join("\n");
  }
  const key = JSON.stringify(apiKey ?? "LOCAL_API_KEY");
  if (kind === "decisions") {
    // The TypeSafe SDKs take the API root and append /v1/systemone themselves.
    const baseUrl = JSON.stringify(url.origin);
    if (language === "python") return `# pip install typesafe-sdk\nfrom typesafe_sdk import Noul, TypeSafeClient\n\nwith TypeSafeClient(\n    api_key=${key},\n    base_url=${baseUrl},\n) as client:\n    response = client.system_one(\n        model=${JSON.stringify(model)},\n        state={"ticket": "App crashes on login after update"},\n        questions={"bug": Noul(instructions="Is this a bug report?")},\n    )\nprint(response.nouls["bug"].noul)`;
    return `// npm install @typesafe-ai/sdk\nimport { noul, TypeSafeClient } from "@typesafe-ai/sdk";\n\nconst client = new TypeSafeClient({\n  baseURL: ${baseUrl},\n  apiKey: ${key},\n});\n\nconst response = await client.systemOne({\n  model: ${JSON.stringify(model)},\n  state: { ticket: "App crashes on login after update" },\n  questions: { bug: noul("Is this a bug report?") },\n});\nconsole.log(response.answers.bug.noul);`;
  }
  const baseUrl = JSON.stringify(new URL("/v1", endpoint).toString());
  const name = JSON.stringify(model);
  if (language === "python") {
    const client = `from openai import OpenAI\n\nclient = OpenAI(\n    api_key=${key},\n    base_url=${baseUrl},\n)\n\n`;
    if (kind === "images") return `import base64\n\n${client}response = client.images.generate(\n    model=${name},\n    prompt=${JSON.stringify(imagePrompt)},\n    n=1,\n    size="512x512",\n)\nwith open("${imageFile}", "wb") as image:\n    image.write(base64.b64decode(response.data[0].b64_json))\nprint("Saved ${imageFile}")`;
    if (kind === "transcription") return `${client}with open("${audioFile}", "rb") as audio:\n    transcription = client.audio.transcriptions.create(\n        model=${name},\n        file=audio,\n        response_format="json",\n    )\nprint(transcription.text)`;
    return `${client}response = client.chat.completions.create(\n    model=${name},\n    messages=[{"role": "user", "content": "Hello"}],\n)\nprint(response.choices[0].message.content)`;
  }
  const client = `import OpenAI from "openai";\n\nconst client = new OpenAI({\n  baseURL: ${baseUrl},\n  apiKey: ${key},\n});\n\n`;
  if (kind === "images") return `import { writeFile } from "node:fs/promises";\n${client}const response = await client.images.generate(${payload});\nawait writeFile("${imageFile}", Buffer.from(response.data[0].b64_json, "base64"));\nconsole.log("Saved ${imageFile}");`;
  if (kind === "transcription") return `import fs from "node:fs";\n${client}const transcription = await client.audio.transcriptions.create({\n  model: ${name},\n  file: fs.createReadStream("${audioFile}"),\n  response_format: "json",\n});\nconsole.log(transcription.text);`;
  return `${client}const response = await client.chat.completions.create(${payload});\nconsole.log(response.choices[0].message.content);`;
}
