export type ExampleLanguage = "curl" | "python" | "javascript";

export function localApiExample(language: ExampleLanguage, endpoint: string, model: string, apiKey?: string): string {
  const url = new URL("/v1/chat/completions", endpoint);
  if (url.protocol !== "http:" && url.protocol !== "https:") throw new Error("Invalid Local API endpoint");
  const payload = JSON.stringify({ model, messages: [{ role: "user", content: "Hello" }] }, null, 2);
  const baseUrl = JSON.stringify(new URL("/v1", endpoint).toString());
  if (language === "curl") {
    const quotedUrl = `'${url.toString().replaceAll("'", "'\\''")}'`;
    return [
      `curl --fail-with-body --max-time 60 --request POST ${quotedUrl} \\`,
      apiKey ? `  --header 'Authorization: Bearer ${apiKey.replaceAll("'", "'\\''")}' \\` : '  --header "Authorization: Bearer ${PAG_API_KEY:?Set PAG_API_KEY to your local client key}" \\',
      "  --header 'Content-Type: application/json' \\",
      "  --data @- <<'JSON'",
      payload,
      "JSON",
    ].join("\n");
  }
  if (language === "python") return `from openai import OpenAI\n\nclient = OpenAI(\n    api_key=${JSON.stringify(apiKey ?? "LOCAL_API_KEY")},\n    base_url=${baseUrl},\n)\n\nresponse = client.chat.completions.create(\n    model=${JSON.stringify(model)},\n    messages=[{"role": "user", "content": "Hello"}],\n)\nprint(response.choices[0].message.content)`;
  return `import OpenAI from "openai";\n\nconst client = new OpenAI({\n  baseURL: ${baseUrl},\n  apiKey: ${JSON.stringify(apiKey ?? "LOCAL_API_KEY")},\n});\n\nconst response = await client.chat.completions.create(${payload});\nconsole.log(response.choices[0].message.content);`;
}
