import { expect, test } from "bun:test";
import { createOpencodeClient } from "@opencode-ai/sdk";
import type { Config, PluginInput } from "@opencode-ai/plugin";

import plugin from "../src/index.ts";

const input: PluginInput = {
  client: createOpencodeClient({ baseUrl: "http://127.0.0.1:4096" }),
  project: {
    id: "test",
    worktree: "/tmp/opencode-provider-aci-test",
    vcs: "git",
    time: { created: 0 },
  },
  directory: "/tmp/opencode-provider-aci-test",
  worktree: "/tmp/opencode-provider-aci-test",
  experimental_workspace: { register() {} },
  serverUrl: new URL("http://127.0.0.1:4096"),
  $: Bun.$,
};

test("keeps an existing provider blocked when configuration fails", async () => {
  const hooks = await plugin.server(input, {
    baseURL: "http://unverified.example/v1",
  });
  const config: Config = {
    provider: {
      aci: {
        npm: "@ai-sdk/openai-compatible",
        options: { baseURL: "http://unverified.example/v1", apiKey: "test-only" },
        models: { test: { name: "Test" } },
      },
    },
  };

  await expect(hooks.config?.(config)).rejects.toThrow("expected an https URL");
  const fetch = config.provider?.aci?.options?.fetch;
  expect(typeof fetch).toBe("function");
  if (typeof fetch !== "function") throw new Error("secure fetch was not installed");
  await expect(fetch("http://unverified.example/v1/chat/completions")).rejects.toThrow(
    "ACI provider blocked",
  );
});
