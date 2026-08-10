import assert from "node:assert/strict";
import { test } from "node:test";

import { loadAciCloudConfig } from "../src/config.ts";

// Tests run under the neutral default profile (envPrefix "ACI", no default
// base URL). Branded shells apply their profile before loading.

test("defaults: TEE-only, auto-verify, pinning on, fail-closed", () => {
  const config = loadAciCloudConfig({ env: {} });
  assert.equal(config.models.isTeeOnly, true);
  assert.equal(config.verify.autoFetchReceipt, true);
  assert.equal(config.verify.requireAttestationMatch, false);
  assert.equal(config.verify.failOpenOnUnpinned, false);
  assert.equal(config.pinning.enabled, true);
  assert.equal(config.baseUrl, "");
});

test("plugin options layer applies flat keys", () => {
  const config = loadAciCloudConfig({
    env: {},
    pluginOptions: {
      baseUrl: "https://gateway.example/v1/",
      isTeeOnly: false,
      failOpenOnUnpinned: true,
      pinning: false,
    },
  });
  assert.equal(config.baseUrl, "https://gateway.example/v1");
  assert.equal(config.models.isTeeOnly, false);
  assert.equal(config.verify.failOpenOnUnpinned, true);
  assert.equal(config.pinning.enabled, false);
});

test("env layer beats plugin options", () => {
  const config = loadAciCloudConfig({
    env: { ACI_BASE_URL: "https://env.example/v1", ACI_IS_TEE_ONLY: "true" },
    pluginOptions: { baseUrl: "https://plugin.example/v1", isTeeOnly: false },
  });
  assert.equal(config.baseUrl, "https://env.example/v1");
  assert.equal(config.models.isTeeOnly, true);
});

test("runtime overrides beat env", () => {
  const config = loadAciCloudConfig({
    env: { ACI_BASE_URL: "https://env.example/v1", ACI_FAIL_OPEN_ON_UNPINNED: "false" },
    overrides: {
      baseUrl: "https://runtime.example/v1",
      verify: { failOpenOnUnpinned: true },
    },
  });
  assert.equal(config.baseUrl, "https://runtime.example/v1");
  assert.equal(config.verify.failOpenOnUnpinned, true);
});

test("env allowlist parses comma-separated ids", () => {
  const config = loadAciCloudConfig({
    env: { ACI_MODEL_ALLOWLIST: "a/model, b/model ,c/model" },
  });
  assert.deepEqual(config.models.allowlist, ["a/model", "b/model", "c/model"]);
});

test("invalid values are ignored, never partially applied", () => {
  const config = loadAciCloudConfig({
    env: {},
    pluginOptions: { isTeeOnly: "maybe", pinning: 42, baseUrl: 7 },
  });
  assert.equal(config.models.isTeeOnly, true);
  assert.equal(config.pinning.enabled, true);
  assert.equal(config.baseUrl, "");
});
