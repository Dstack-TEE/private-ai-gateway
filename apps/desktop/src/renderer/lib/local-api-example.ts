export type ExampleLanguage = "curl" | "python" | "javascript";

export function localApiExample(language: ExampleLanguage, endpoint: string, model: string, apiKey?: string): string {
  const url = new URL("/v1/chat/completions", endpoint);
  if (url.protocol !== "http:" && url.protocol !== "https:") throw new Error("Invalid Local API endpoint");
  const payload = JSON.stringify({ model, messages: [{ role: "user", content: "Hello" }] }, null, 2);
  const address = JSON.stringify(url.toString());
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
  if (language === "python") return `import json\nimport os\nfrom urllib.request import Request, urlopen\n\nbody = ${payload}\nrequest = Request(\n    ${address},\n    data=json.dumps(body).encode(),\n    headers={\n        "Authorization": ${apiKey ? JSON.stringify(`Bearer ${apiKey}`) : `f"Bearer {os.environ['PAG_API_KEY']}"`},\n        "Content-Type": "application/json",\n    },\n    method="POST",\n)\nwith urlopen(request, timeout=60) as response:\n    print(json.load(response))`;
  return `// Node.js 18+\nconst apiKey = ${apiKey ? JSON.stringify(apiKey) : "process.env.PAG_API_KEY"};\nif (!apiKey) throw new Error("Set PAG_API_KEY to your local client key.");\n\nconst response = await fetch(${address}, {\n  method: "POST",\n  signal: AbortSignal.timeout(60_000),\n  headers: {\n    Authorization: \`Bearer \${apiKey}\`,\n    "Content-Type": "application/json",\n  },\n  body: JSON.stringify(${payload}),\n});\nif (!response.ok) throw new Error(\`HTTP \${response.status}\`);\nconsole.log(await response.json());`;
}
