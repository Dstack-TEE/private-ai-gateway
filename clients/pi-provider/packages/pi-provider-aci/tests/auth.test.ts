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
