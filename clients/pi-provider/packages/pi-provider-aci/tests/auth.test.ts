import assert from "node:assert/strict";
import { test } from "node:test";

import { createApiKeyAuth } from "../src/auth.ts";
import { resolveProfile } from "../src/profile.ts";

test("native auth prefers stored credentials over the configured API key environment", async () => {
  const auth = createApiKeyAuth(resolveProfile({ apiKeyEnv: "PROVIDER_API_KEY" }));
  const values: Record<string, string | undefined> = {
    PROVIDER_API_KEY: "environment",
  };
  const ctx = {
    env: async (name: string) => values[name],
    fileExists: async () => false,
  };
  const signal = new AbortController().signal;

  assert.deepEqual(
    await auth.resolve({ ctx, credential: { type: "api_key", key: "stored" }, signal }),
    { auth: { apiKey: "stored" }, env: undefined, source: "stored credential" },
  );
  assert.deepEqual(await auth.resolve({ ctx, signal }), {
    auth: { apiKey: "environment" },
    source: "PROVIDER_API_KEY",
  });
});

test("branded account login remains optional alongside the native API-key prompt", async () => {
  let accountLogins = 0;
  const notifications: unknown[] = [];
  const auth = createApiKeyAuth(
    resolveProfile({
      providerId: "brand",
      label: "Brand Cloud",
      apiKeyEnv: "BRAND_API_KEY",
    }),
    {
      label: "Brand Cloud account",
      async start() {
        return {
          url: "https://brand.test/device",
          instructions: "Approve the device login",
          presentation: {
            type: "device_code" as const,
            userCode: "ABCD-EFGH",
            intervalSeconds: 2,
            expiresInSeconds: 60,
          },
          async complete(options) {
            accountLogins += 1;
            options?.onProgress?.("Waiting for authorization...");
            return { apiKey: "account-issued" };
          },
        };
      },
    },
  );
  const prompts: unknown[] = [];
  const signal = new AbortController().signal;

  const account = await auth.login?.({
    signal,
    notify(event) {
      notifications.push(event);
    },
    async prompt(prompt) {
      prompts.push(prompt);
      return "account";
    },
  });
  assert.deepEqual(account, { type: "api_key", key: "account-issued" });
  assert.equal(accountLogins, 1);
  assert.deepEqual(notifications, [
    {
      type: "device_code",
      userCode: "ABCD-EFGH",
      verificationUri: "https://brand.test/device",
      intervalSeconds: 2,
      expiresInSeconds: 60,
    },
    { type: "progress", message: "Waiting for authorization..." },
  ]);
  assert.deepEqual(prompts[0], {
    type: "select",
    message: "Log in to Brand Cloud",
    options: [
      { id: "account", label: "Brand Cloud account" },
      { id: "api-key", label: "Brand Cloud API key" },
    ],
  });

  const manual = await auth.login?.({
    signal,
    notify() {},
    async prompt(prompt) {
      if (prompt.type === "select") return "api-key";
      assert.equal(prompt.type, "secret");
      assert.equal(prompt.message, "Enter Brand Cloud API key");
      return "manual-key";
    },
  });
  assert.deepEqual(manual, { type: "api_key", key: "manual-key" });
  assert.equal(accountLogins, 1);
});
