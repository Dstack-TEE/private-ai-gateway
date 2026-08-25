import assert from "node:assert/strict";
import { test } from "node:test";

import {
  DEFAULT_ACI_CLOUD_CONFIG,
  validateAciCloudConfig,
  type AciCloudConfig,
} from "../src/config.ts";

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
  assert.equal(validated.models.thinkingFormat, "auto");
});

test("validateAciCloudConfig: rejects invalid thinkingFormat", () => {
  const bad = { ...BASE, models: { ...BASE.models, thinkingFormat: "bogus" } };
  assert.throws(
    () => validateAciCloudConfig(bad),
    /expected "auto" \| "qwen" \| "openai" \| "off"/,
  );
});

test("validateAciCloudConfig: rejects non-boolean isTeeOnly", () => {
  const bad = { ...BASE, models: { ...BASE.models, isTeeOnly: "yes" } };
  assert.throws(() => validateAciCloudConfig(bad), /expected a boolean/);
});

test("validateAciCloudConfig: rejects empty baseUrl", () => {
  const bad = { ...BASE, baseUrl: "" };
  assert.throws(() => validateAciCloudConfig(bad), /expected a non-empty string/);
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

test("validateAciCloudConfig: defaultModel is optional", () => {
  const validated = validateAciCloudConfig({
    ...BASE,
    defaultModel: "aci/test-model",
  });
  assert.equal(validated.defaultModel, "aci/test-model");
});
