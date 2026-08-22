import assert from "node:assert/strict";
import { test } from "node:test";

import type { Hooks } from "@opencode-ai/plugin";

import { createProvider } from "../core.ts";

function mockInput() {
  const logs: string[] = [];
  const toasts: string[] = [];
  const input = {
    client: {
      app: {
        log: async ({ body }: { body: { message: string } }) => {
          logs.push(body.message);
          return {};
        },
      },
      tui: {
        showToast: async ({ body }: { body: { message: string } }) => {
          toasts.push(body.message);
          return {};
        },
      },
    },
    project: {},
    directory: "/tmp",
    worktree: "/tmp",
    serverUrl: new URL("http://localhost:0"),
  };
  return { input: input as never, logs, toasts };
}

test("plugin assembles hooks and injects the provider with a custom fetch", async () => {
  const plugin = createProvider();
  const { input } = mockInput();
  const hooks: Hooks = await plugin(input, {});

  assert.equal(typeof hooks.config, "function");
  assert.equal(typeof hooks.tool?.["aci_verification_status"], "object");
  // Neutral profile has no oauth block.
  assert.equal(hooks.auth, undefined);

  const cfg: { provider?: Record<string, never> } = {};
  await hooks.config!(cfg as never);

  const provider = (cfg as Record<string, Record<string, Record<string, unknown>>>).provider.aci;
  assert.ok(provider, "provider.aci registered");
  assert.equal(provider.npm, "@ai-sdk/openai-compatible");
  assert.equal(provider.name, "Private AI Gateway");
  const options = provider.options as Record<string, unknown>;
  assert.equal(typeof options.fetch, "function");
  // No env key in the test environment: apiKey must be omitted so opencode
  // fills it from the auth loader's provider.key (a stale template would
  // shadow the logged-in credential).
  assert.equal(options.apiKey, undefined);
});

test("config hook respects user-declared provider options and models", async () => {
  const plugin = createProvider();
  const { input } = mockInput();
  const hooks = await plugin(input, {});

  const cfg = {
    provider: {
      aci: {
        options: { baseURL: "https://user.example/v1", headers: { "x-custom": "1" } },
        models: { "user/model": { name: "User Model" } },
      },
    },
  };
  await hooks.config!(cfg as never);

  const provider = cfg.provider.aci as unknown as {
    options: Record<string, unknown>;
    models: Record<string, unknown>;
  };
  assert.equal(provider.options.baseURL, "https://user.example/v1");
  assert.deepEqual(provider.options.headers, { "x-custom": "1" });
  assert.equal(typeof provider.options.fetch, "function");
  assert.ok(provider.models["user/model"], "user model preserved");
});

test("oauth profile adds the auth hook; loader captures the credential", async () => {
  const plugin = createProvider({
    oauth: {
      name: "Test Brand",
      startDeviceFlow: async () => ({
        deviceCode: "dc",
        userCode: "UC-123",
        verificationUri: "https://brand.example/device",
        intervalSeconds: 1,
        expiresInSeconds: 60,
      }),
      pollDeviceFlow: async () => ({
        access: "minted-key",
        refresh: "",
        expires: Date.now() + 1e9,
      }),
    },
  });
  const { input } = mockInput();
  const hooks = await plugin(input, {});

  assert.ok(hooks.auth, "auth hook present");
  assert.equal(hooks.auth.provider, "aci");
  assert.equal(hooks.auth.methods.length, 2);

  const loaded = await hooks.auth.loader!(
    async () => ({ type: "oauth", access: "oauth-key" }) as never,
    {} as never,
  );
  assert.deepEqual(loaded, { apiKey: "oauth-key" });

  // The device-flow method maps onto authorize() -> auto callback polling.
  const oauthMethod = hooks.auth.methods[0];
  assert.equal(oauthMethod.type, "oauth");
  if (oauthMethod.type !== "oauth") return;
  const result = await oauthMethod.authorize();
  assert.equal(result.method, "auto");
  assert.match(result.instructions, /UC-123/);
  if (result.method !== "auto") return;
  const callbackResult = await result.callback();
  assert.equal(callbackResult.type, "success");
  if (callbackResult.type !== "success" || !("access" in callbackResult)) return;
  assert.equal(callbackResult.access, "minted-key");
});

test("verification status tool reports pinning and receipt state", async () => {
  const plugin = createProvider();
  const { input } = mockInput();
  const hooks = await plugin(input, {});
  const statusTool = hooks.tool!["aci_verification_status"];
  const result = await statusTool.execute({}, {} as never);
  const report = JSON.parse(typeof result === "string" ? result : result.output);
  assert.equal(report.provider, "aci");
  assert.equal(typeof report.status, "string");
  assert.ok(report.pinning);
});
