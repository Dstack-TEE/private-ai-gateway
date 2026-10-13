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
    // System One is native buffered JSON, so the examples call it over plain HTTP.
    const authorization = JSON.stringify(`Bearer ${apiKey ?? "LOCAL_API_KEY"}`);
    if (language === "python") return `import requests\n\nresponse = requests.post(\n    ${JSON.stringify(url.toString())},\n    headers={"Authorization": ${authorization}},\n    json=${JSON.stringify(body, null, 4).replaceAll("\n", "\n    ")},\n    timeout=60,\n)\nresponse.raise_for_status()\nprint(response.json()["answers"])`;
    return `const response = await fetch(${JSON.stringify(url.toString())}, {\n  method: "POST",\n  headers: {\n    Authorization: ${authorization},\n    "Content-Type": "application/json",\n  },\n  body: JSON.stringify(${payload.replaceAll("\n", "\n  ")}),\n  signal: AbortSignal.timeout(60_000),\n});\nif (!response.ok) throw new Error(\`System One request failed: \${response.status} \${await response.text()}\`);\nconst { answers } = await response.json();\nconsole.log(answers);`;
  }
  const baseUrl = JSON.stringify(new URL("/v1", endpoint).toString());
  if (language === "python") return `from openai import OpenAI\n\nclient = OpenAI(\n    api_key=${JSON.stringify(apiKey ?? "LOCAL_API_KEY")},\n    base_url=${baseUrl},\n)\n\nresponse = client.chat.completions.create(\n    model=${JSON.stringify(model)},\n    messages=[{"role": "user", "content": "Hello"}],\n)\nprint(response.choices[0].message.content)`;
  return `import OpenAI from "openai";\n\nconst client = new OpenAI({\n  baseURL: ${baseUrl},\n  apiKey: ${JSON.stringify(apiKey ?? "LOCAL_API_KEY")},\n});\n\nconst response = await client.chat.completions.create(${payload});\nconsole.log(response.choices[0].message.content);`;
}
