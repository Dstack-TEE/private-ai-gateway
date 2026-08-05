import assert from "node:assert/strict";
import { after, test } from "node:test";

import {
  API_KEY_ENV,
  DEFAULT_BASE_URL,
  FOOTER_STATUS_KEY,
  LOG_PREFIX,
  PROVIDER_ID,
  applyProviderProfile,
  getBaseUrl,
} from "../src/constants.ts";
import { DEFAULT_PROFILE, resolveProfile } from "../src/profile.ts";

const KEEP = {
  apiKey: process.env.ACI_LLM_API_KEY,
  base: process.env.ACI_BASE_URL,
  alias: process.env.PHALA_BASE_URL,
};

after(() => {
  for (const [k, v] of Object.entries(KEEP)) {
    if (v === undefined) delete process.env[k];
    else process.env[k] = v;
  }
});

test("resolveProfile fills neutral defaults for unset fields", () => {
  const p = resolveProfile({ providerId: "brand", defaultBaseUrl: "https://brand.test/v1" });
  assert.equal(p.providerId, "brand");
  assert.equal(p.defaultBaseUrl, "https://brand.test/v1");
  // Untouched fields keep the neutral default.
  assert.equal(p.envPrefix, DEFAULT_PROFILE.envPrefix);
  assert.equal(p.footerKey, DEFAULT_PROFILE.footerKey);
  assert.equal(p.apiKeyEnv, DEFAULT_PROFILE.apiKeyEnv);
});

test("applyProviderProfile updates the identity live-bindings", () => {
  applyProviderProfile({
    providerId: "brand-x",
    label: "Brand X",
    defaultBaseUrl: "https://brand.test/v1",
    apiKeyEnv: "BRAND_X_KEY",
    envPrefix: "BRAND_X",
    footerKey: "brand-x",
    logPrefix: "[brand-x]",
  });
  assert.equal(PROVIDER_ID, "brand-x");
  assert.equal(API_KEY_ENV, "BRAND_X_KEY");
  assert.equal(FOOTER_STATUS_KEY, "brand-x");
  assert.equal(DEFAULT_BASE_URL, "https://brand.test/v1");
  assert.equal(LOG_PREFIX, "[brand-x]");
});

test("getBaseUrl: profile default wins when no env is set", () => {
  delete process.env.ACI_BASE_URL;
  delete process.env.ACI_CLOUD_BASE_URL;
  delete process.env.PHALA_BASE_URL;
  resolveProfile({ envPrefix: "ACI", defaultBaseUrl: "https://default.test/v1", baseUrlAliases: ["PHALA_BASE_URL"] });
  assert.equal(getBaseUrl(), "https://default.test/v1");
});

test("getBaseUrl: prefixed env var overrides the profile default", () => {
  process.env.ACI_BASE_URL = "https://env.test/v1";
  resolveProfile({ envPrefix: "ACI", defaultBaseUrl: "https://default.test/v1" });
  assert.equal(getBaseUrl(), "https://env.test/v1");
});

test("getBaseUrl: brand alias env var is honored", () => {
  delete process.env.ACI_BASE_URL;
  delete process.env.ACI_CLOUD_BASE_URL;
  process.env.PHALA_BASE_URL = "https://alias.test/v1";
  resolveProfile({ envPrefix: "ACI", defaultBaseUrl: "https://default.test/v1", baseUrlAliases: ["PHALA_BASE_URL"] });
  assert.equal(getBaseUrl(), "https://alias.test/v1");
});
