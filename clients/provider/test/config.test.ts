import assert from "node:assert/strict";
import test from "node:test";

import { resolveAciProviderConfig } from "../src/config.ts";
import { resolveAciProviderProfile } from "../src/profile.ts";

const profile = resolveAciProviderProfile({ defaultBaseURL: "https://gateway.example/v1" });

test("resolves provider policy from profile, environment, and explicit options", () => {
  const composeHash = "a".repeat(64);
  const config = resolveAciProviderConfig(
    profile,
    {
      models: { allowlist: ["qwen/qwen3"] },
      receipts: { verification: "response", historySize: 8 },
    },
    {
      ACI_BASE_URL: "https://custom.example/v1",
      ACI_ACCEPTED_COMPOSE_HASHES: composeHash.toUpperCase(),
    },
  );

  assert.equal(config.baseURL, "https://custom.example/v1");
  assert.deepEqual(config.models.allowlist, ["qwen/qwen3"]);
  assert.deepEqual(config.trust.acceptedComposeHashes, [composeHash]);
  assert.deepEqual(config.receipts, { verification: "response", historySize: 8 });
});

test("rejects a non-HTTPS model endpoint", () => {
  assert.throws(
    () => resolveAciProviderConfig(profile, { baseURL: "http://gateway.example/v1" }),
    /expected an https URL/,
  );
});
