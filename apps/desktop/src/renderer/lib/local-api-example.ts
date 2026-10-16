import { join } from "shlex";
import type { ModelSummary } from "../../shared/contracts";

export type ExampleKind = "chat" | "decisions";
export type ExampleLanguage = "curl" | "python" | "javascript";

/** The catalog models an example of this kind can call. */
export function exampleModels(kind: ExampleKind, models: ModelSummary[]): ModelSummary[] {
  return models.filter(kind === "chat"
    ? (model) => model.supportedEndpoints?.includes("/v1/chat/completions") ?? true
    : (model) => model.outputModalities.includes("decisions"));
}

export function localApiExample(kind: ExampleKind, language: ExampleLanguage, endpoint: string, model: string, apiKey?: string): string {
  const url = new URL(kind === "chat" ? "/v1/chat/completions" : "/v1/systemone", endpoint);
  if (url.protocol !== "http:" && url.protocol !== "https:") throw new Error("Invalid Local API endpoint");
  const body = kind === "chat"
    ? { model, messages: [{ role: "user", content: "Hello" }] }
    : { model, state: { ticket: "App crashes on login after update" }, questions: { bug: { type: "noul", instructions: "Is this a bug report?" } } };
  const payload = JSON.stringify(body, null, 2);
  if (language === "curl") {
    return [
      `${join(["curl", "--fail-with-body", "--max-time", "60", "--request", "POST", url.toString()])} \\`,
      // Without a key, the shell supplies it from PAP_API_KEY or stops with the hint.
      apiKey ? `  ${join(["--header", `Authorization: Bearer ${apiKey}`])} \\` : '  --header "Authorization: Bearer ${PAP_API_KEY:?Set PAP_API_KEY to your local client key}" \\',
      `  ${join(["--header", "Content-Type: application/json"])} \\`,
      "  --data @- <<'JSON'",
      payload,
      "JSON",
    ].join("\n");
  }
  if (kind === "decisions") {
    // The TypeSafe SDKs take the API root and append /v1/systemone themselves.
    const baseUrl = JSON.stringify(url.origin);
    const key = JSON.stringify(apiKey ?? "LOCAL_API_KEY");
    if (language === "python") return `# pip install typesafe-sdk\nfrom typesafe_sdk import Noul, TypeSafeClient\n\nwith TypeSafeClient(\n    api_key=${key},\n    base_url=${baseUrl},\n) as client:\n    response = client.system_one(\n        model=${JSON.stringify(model)},\n        state={"ticket": "App crashes on login after update"},\n        questions={"bug": Noul(instructions="Is this a bug report?")},\n    )\nprint(response.nouls["bug"].noul)`;
    return `// npm install @typesafe-ai/sdk\nimport { noul, TypeSafeClient } from "@typesafe-ai/sdk";\n\nconst client = new TypeSafeClient({\n  baseURL: ${baseUrl},\n  apiKey: ${key},\n});\n\nconst response = await client.systemOne({\n  model: ${JSON.stringify(model)},\n  state: { ticket: "App crashes on login after update" },\n  questions: { bug: noul("Is this a bug report?") },\n});\nconsole.log(response.answers.bug.noul);`;
  }
  const baseUrl = JSON.stringify(new URL("/v1", endpoint).toString());
  if (language === "python") return `from openai import OpenAI\n\nclient = OpenAI(\n    api_key=${JSON.stringify(apiKey ?? "LOCAL_API_KEY")},\n    base_url=${baseUrl},\n)\n\nresponse = client.chat.completions.create(\n    model=${JSON.stringify(model)},\n    messages=[{"role": "user", "content": "Hello"}],\n)\nprint(response.choices[0].message.content)`;
  return `import OpenAI from "openai";\n\nconst client = new OpenAI({\n  baseURL: ${baseUrl},\n  apiKey: ${JSON.stringify(apiKey ?? "LOCAL_API_KEY")},\n});\n\nconst response = await client.chat.completions.create(${payload});\nconsole.log(response.choices[0].message.content);`;
}
