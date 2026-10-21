// Resolve the installers' release channels once, on the host, so every
// installed version can be checked and the same inputs reported on failure.
import { readFile } from "node:fs/promises";
import { setTimeout } from "node:timers/promises";
import { parseEnv } from "node:util";

const npmPackages = {
  CODEX_VERSION: "@openai/codex",
  OPENCODE_VERSION: "opencode-ai",
  PI_VERSION: "@earendil-works/pi-coding-agent",
  OH_MY_PI_VERSION: "@oh-my-pi/pi-coding-agent",
  DSH_VERSION: "@deepseek-ai/dsh",
  OPENCLAW_VERSION: "openclaw",
};

async function request(url, options) {
  for (let attempt = 0; ; attempt++) {
    let response;
    try {
      response = await fetch(url, { ...options, signal: AbortSignal.timeout(30_000) });
    } catch (error) {
      if (attempt === 3) throw new Error(`${url}: ${error.message}`, { cause: error });
    }
    if (response) {
      if (response.ok) return response;
      await response.body?.cancel();
      if (attempt === 3 || (response.status !== 429 && response.status < 500)) {
        throw new Error(`${url}: HTTP ${response.status}`);
      }
    }
    await setTimeout(1_000 * 2 ** attempt);
  }
}

async function latest(name) {
  const pkg = npmPackages[name];
  if (pkg) {
    const response = await request(`https://registry.npmjs.org/-/package/${encodeURIComponent(pkg)}/dist-tags`);
    return (await response.json()).latest;
  }
  switch (name) {
    case "NODE_VERSION": {
      const response = await request("https://nodejs.org/dist/index.json");
      return (await response.json()).find(({ version, files }) =>
        /^v24\.\d+\.\d+$/.test(version) && files.includes("linux-x64"))?.version.slice(1);
    }
    case "CLAUDE_CODE_VERSION":
      return (await (await request("https://downloads.claude.ai/claude-code-releases/latest")).text()).trim();
    case "OPENCODE_V2_VERSION":
      return (await (await request("https://opencode.ai/update/api/latest/cli/npm")).json()).version;
    case "BUN_VERSION":
    case "HERMES_VERSION": {
      const repo = name === "BUN_VERSION" ? "oven-sh/bun" : "NousResearch/hermes-agent";
      // GitHub's latest-release redirect also works without an API token.
      const response = await request(`https://github.com/${repo}/releases/latest`, { method: "HEAD" });
      const prefix = `https://github.com/${repo}/releases/tag/`;
      if (!response.url.startsWith(prefix)) throw new Error(`No latest release for ${repo}`);
      const tag = response.url.slice(prefix.length);
      return name === "BUN_VERSION" ? tag.replace(/^bun-v/, "") : tag;
    }
    default:
      throw new Error(`Unknown version setting ${name}`);
  }
}

try {
  const defaults = parseEnv(await readFile(new URL("versions.env", import.meta.url), "utf8"));
  const versions = await Promise.all(Object.entries(defaults).map(async ([name, fallback]) => {
    const requested = process.env[name] ?? fallback;
    const resolved = requested === "latest" ? await latest(name) : requested;
    const pattern = name === "HERMES_VERSION" ? /^v\d+\.\d+\.\d+(?:[-.][0-9A-Za-z.-]+)?$/
      : /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;
    if (typeof resolved !== "string" || !pattern.test(resolved)) {
      throw new Error(`Invalid ${name}: expected an exact release version, got ${JSON.stringify(resolved)}`);
    }
    return `${name}=${resolved}`;
  }));
  console.log(versions.join("\n"));
} catch (error) {
  console.error(`Resolving versions failed: ${error.message}`);
  process.exitCode = 1;
}
