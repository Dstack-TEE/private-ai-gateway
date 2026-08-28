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
  const inspect = hooks.tool?.aci_inspect;
  expect(inspect).toBeDefined();
  if (!inspect) throw new Error("ACI inspection tool was not registered");
  await expect(
    inspect.execute(
      { action: "status" },
      {
        sessionID: "test",
        messageID: "test",
        agent: "test",
        directory: input.directory,
        worktree: input.worktree,
        abort: new AbortController().signal,
        metadata() {},
        async ask() {},
      },
    ),
  ).rejects.toThrow("not connected to a verified gateway");
  const config: Config = {
    command: {
      "aci-attestation": {
        description: "User command",
        template: "Keep this command",
      },
    },
    provider: {
      aci: {
        npm: "@ai-sdk/openai-compatible",
        options: { baseURL: "http://unverified.example/v1", apiKey: "test-only" },
        models: { test: { name: "Test" } },
      },
    },
  };

  await expect(hooks.config?.(config)).rejects.toThrow("expected an https URL");
  expect(config.command?.["aci-attestation"]?.template).toBe("Keep this command");
  expect(config.command?.["aci-receipts"]?.template).toContain(
    'aci_inspect tool exactly once with action "receipts"',
  );
  expect(config.command?.["aci-receipt"]?.template).toContain('If "$1" is empty, omit id');
  expect(config.command?.["aci-session"]?.template).toContain('Pass "$1" exactly as id');
  expect(config.provider?.aci?.env).toEqual(["ACI_API_KEY"]);
  const fetch = config.provider?.aci?.options?.fetch;
  expect(typeof fetch).toBe("function");
  if (typeof fetch !== "function") throw new Error("secure fetch was not installed");
  await expect(fetch("http://unverified.example/v1/chat/completions")).rejects.toThrow(
    "ACI provider blocked",
  );
});
