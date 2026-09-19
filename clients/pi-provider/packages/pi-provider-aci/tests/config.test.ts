import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";

import {
  DEFAULT_ACI_CLOUD_CONFIG,
  getGlobalAciCloudConfigPath,
  loadAciCloudConfig,
  loadHomeAciCloudConfig,
  toAciProviderConfig,
  validateAciCloudConfig,
  type AciCloudConfig,
} from "../src/config.ts";
import { DEFAULT_PROFILE } from "../src/profile.ts";

// The neutral core leaves baseUrl operator-set (empty default); these tests
// validate the config *shape*, so use an explicit host.
const BASE: AciCloudConfig = {
  ...DEFAULT_ACI_CLOUD_CONFIG,
  baseUrl: "https://gateway.test/v1",
};

test("validating a concrete config passes and preserves values", () => {
  const validated = validateAciCloudConfig(BASE);
  assert.equal(validated.baseUrl, "https://gateway.test/v1");
  assert.equal(validated.models.isTeeOnly, true);
  assert.deepEqual(validated.trust, {});
  assert.equal(toAciProviderConfig(validated).receipts.verification, "response");
});

test("validateAciCloudConfig: normalizes accepted compose hashes", () => {
  const validated = validateAciCloudConfig({
    ...BASE,
    trust: { acceptedComposeHashes: ["AB".repeat(32)] },
  });
  assert.deepEqual(validated.trust.acceptedComposeHashes, ["ab".repeat(32)]);
});

test("validateAciCloudConfig: rejects malformed compose hashes", () => {
  const bad = { ...BASE, trust: { acceptedComposeHashes: ["not-a-hash"] } };
  assert.throws(() => validateAciCloudConfig(bad), /64-character SHA-256 digest/);
});

test("validateAciCloudConfig: rejects empty trust policies", () => {
  for (const trust of [{ acceptedComposeHashes: [] }, { acceptedSessionIds: [] }]) {
    assert.throws(
      () => validateAciCloudConfig({ ...BASE, trust }),
      /expected a non-empty string array/,
    );
  }
});

test("validateAciCloudConfig: accepts only canonical attested-session ids", () => {
  const validated = validateAciCloudConfig({
    ...BASE,
    trust: { acceptedSessionIds: ["ab".repeat(32)] },
  });
  assert.deepEqual(validated.trust.acceptedSessionIds, ["ab".repeat(32)]);

  const bad = { ...BASE, trust: { acceptedSessionIds: ["AB".repeat(32)] } };
  assert.throws(() => validateAciCloudConfig(bad), /lowercase session id/);
});

test("validateAciCloudConfig: rejects non-boolean isTeeOnly", () => {
  const bad = { ...BASE, models: { ...BASE.models, isTeeOnly: "yes" } };
  assert.throws(() => validateAciCloudConfig(bad), /expected a boolean/);
});

test("validateAciCloudConfig: rejects empty baseUrl", () => {
  const bad = { ...BASE, baseUrl: "" };
  assert.throws(() => validateAciCloudConfig(bad), /expected a non-empty URL/);
});

test("validateAciCloudConfig: requires the concrete persisted shape", () => {
  assert.throws(
    () => validateAciCloudConfig({ models: BASE.models, trust: {} }),
    /baseUrl.*required field is missing/,
  );
});

test("validateAciCloudConfig: accepts optional allowlist of non-empty strings", () => {
  const config: AciCloudConfig = {
    ...BASE,
    models: { ...BASE.models, allowlist: ["aci/test-model"] },
  };
  const validated = validateAciCloudConfig(config);
  assert.deepEqual(validated.models.allowlist, ["aci/test-model"]);
});

test("validateAciCloudConfig: allowlist with empty string is rejected", () => {
  const bad = {
    ...BASE,
    models: { ...BASE.models, allowlist: [""] },
  };
  assert.throws(() => validateAciCloudConfig(bad), /expected a non-empty string/);
});

test("validateAciCloudConfig: accepts per-model overrides and carries them through", () => {
  const config = {
    ...BASE,
    models: {
      ...BASE.models,
      overrides: {
        "qwen/qwen3.8-27b": { supportsDeveloperRole: false },
        "openai/gpt-oss-120b": { thinkingLevelMap: { off: null, high: "xhigh" } },
        "qwen/qwen3-vl-30b-a3b-instruct": { maxTokens: 16384 },
      },
    },
  };
  const validated = validateAciCloudConfig(config);
  assert.deepEqual(validated.models.overrides, config.models.overrides);
});

test("validateAciCloudConfig: rejects malformed overrides", () => {
  const badLevel = {
    ...BASE,
    models: { ...BASE.models, overrides: { "m/1": { thinkingLevelMap: { ultra: "x" } } } },
  };
  assert.throws(() => validateAciCloudConfig(badLevel), /unknown thinking level/);

  const badType = {
    ...BASE,
    models: { ...BASE.models, overrides: { "m/1": { supportsDeveloperRole: "yes" } } },
  };
  assert.throws(
    () => validateAciCloudConfig(badType),
    /\/models\/overrides\/m\/1\/supportsDeveloperRole: expected a boolean/,
  );

  const badTokens = {
    ...BASE,
    models: { ...BASE.models, overrides: { "m/1": { maxTokens: -5 } } },
  };
  assert.throws(
    () => validateAciCloudConfig(badTokens),
    /\/models\/overrides\/m\/1\/maxTokens: expected a positive integer/,
  );
});

test("validateAciCloudConfig: receipts verification defaults to response and accepts on-demand", () => {
  const validated = validateAciCloudConfig(BASE);
  assert.deepEqual(validated.receipts, { verification: "response" });

  const onDemand = validateAciCloudConfig({
    ...BASE,
    receipts: { verification: "on-demand" },
  });
  assert.deepEqual(onDemand.receipts, { verification: "on-demand" });
});

test("validateAciCloudConfig: rejects unknown receipts verification modes", () => {
  const bad = { ...BASE, receipts: { verification: "never" } };
  assert.throws(
    () => validateAciCloudConfig(bad),
    /\/receipts\/verification: expected "response" or "on-demand"/,
  );
  const missing = { ...BASE, receipts: {} };
  assert.throws(() => validateAciCloudConfig(missing), /required field is missing/);
});

test("loadAciCloudConfig: env prefix overrides receipts verification", () => {
  const config = loadAciCloudConfig(
    {
      cwd: "/nonexistent-project",
      home: "/nonexistent-home",
      env: { ACI_RECEIPTS_VERIFICATION: "on-demand" },
      profile: DEFAULT_PROFILE,
    },
    { baseUrl: "https://gw.example/v1" },
  );
  assert.equal(config.receipts.verification, "on-demand");
});

test("loadAciCloudConfig: home config file wins over env for receipts verification", () => {
  const home = mkdtempSync(join(tmpdir(), "pi-provider-aci-"));
  try {
    const path = getGlobalAciCloudConfigPath(home);
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(
      path,
      JSON.stringify({
        baseUrl: "https://gw.example/v1",
        models: { isTeeOnly: true },
        trust: {},
        receipts: { verification: "response" },
      }),
    );
    const config = loadAciCloudConfig(
      {
        cwd: "/nonexistent-project",
        home,
        env: { ACI_RECEIPTS_VERIFICATION: "on-demand" },
        profile: DEFAULT_PROFILE,
      },
      undefined,
    );
    // Env sits above home in the layer order, so it wins.
    assert.equal(config.receipts.verification, "on-demand");
  } finally {
    rmSync(home, { recursive: true, force: true });
  }
});

test("loadHomeAciCloudConfig rejects a malformed persisted config", (t) => {
  const home = mkdtempSync(join(tmpdir(), "pi-provider-aci-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const path = getGlobalAciCloudConfigPath(home);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, "{");

  assert.throws(() => loadHomeAciCloudConfig(home), /invalid JSON/);
});
