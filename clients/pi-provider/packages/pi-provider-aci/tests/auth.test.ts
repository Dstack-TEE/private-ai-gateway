import assert from "node:assert/strict";
import { test } from "node:test";

import { createApiKeyAuth } from "../src/auth.ts";
import { resolveProfile } from "../src/profile.ts";

test("native auth resolves stored, primary, and alias API keys in order", async () => {
  const auth = createApiKeyAuth(
    resolveProfile({
      apiKeyEnv: "PRIMARY_KEY",
      apiKeyAliases: ["ALIAS_KEY"],
    }),
  );
  const values: Record<string, string | undefined> = {
    PRIMARY_KEY: "primary",
    ALIAS_KEY: "alias",
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
    auth: { apiKey: "primary" },
    source: "PRIMARY_KEY",
  });
  values.PRIMARY_KEY = undefined;
  assert.deepEqual(await auth.resolve({ ctx, signal }), {
    auth: { apiKey: "alias" },
    source: "ALIAS_KEY",
  });
});
