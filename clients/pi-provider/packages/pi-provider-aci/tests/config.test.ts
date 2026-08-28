import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";

import {
  DEFAULT_ACI_CLOUD_CONFIG,
  getGlobalAciCloudConfigPath,
  loadHomeAciCloudConfig,
  toAciProviderConfig,
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

test("loadHomeAciCloudConfig rejects a malformed persisted config", (t) => {
  const home = mkdtempSync(join(tmpdir(), "pi-provider-aci-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const path = getGlobalAciCloudConfigPath(home);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, "{");

  assert.throws(() => loadHomeAciCloudConfig(home), /invalid JSON/);
});
