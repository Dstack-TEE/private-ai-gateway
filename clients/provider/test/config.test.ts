import assert from "node:assert/strict";
import test from "node:test";

import { aciProviderConfigInputFromEnv, resolveAciProviderConfig } from "../src/config.ts";
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

test("uses one deterministic base URL environment priority for every adapter", () => {
  const env = {
    ACI_BASE_URL: "https://base.example/v1",
    ACI_CLOUD_API_PREFIX: "https://prefix.example/v1",
    ACI_CLOUD_BASE_URL: "https://cloud.example/v1",
  };

  assert.equal(aciProviderConfigInputFromEnv(profile, env).baseURL, env.ACI_BASE_URL);
  assert.equal(resolveAciProviderConfig(profile, {}, env).baseURL, env.ACI_BASE_URL);
});

test("rejects an invalid boolean environment value", () => {
  assert.throws(
    () => resolveAciProviderConfig(profile, {}, { ACI_IS_TEE_ONLY: "sometimes" }),
    /expected a boolean/,
  );
});

test("rejects invalid explicit values instead of falling back to profile defaults", () => {
  assert.throws(() => resolveAciProviderConfig(profile, { baseURL: 42 }), /non-empty URL/);
  assert.throws(
    () => resolveAciProviderConfig(profile, { models: { isTeeOnly: null } }),
    /expected a boolean/,
  );
});

test("validates trust policy supplied by a provider profile", () => {
  assert.throws(
    () =>
      resolveAciProviderConfig(
        resolveAciProviderProfile({
          defaultBaseURL: "https://gateway.example/v1",
          acceptedComposeHashes: ["not-a-compose-hash"],
        }),
      ),
    /expected a 64-character SHA-256 digest/,
  );
  assert.throws(
    () =>
      resolveAciProviderConfig(
        resolveAciProviderProfile({
          defaultBaseURL: "https://gateway.example/v1",
          acceptedSessionIds: ["A".repeat(64)],
        }),
      ),
    /expected a 64-character lowercase session id/,
  );
});
