import assert from "node:assert/strict";
import { test } from "node:test";

import { getGlobalAciCloudConfigPath } from "../src/config.ts";
import { getBaseUrl } from "../src/constants.ts";
import { DEFAULT_PROFILE, resolveProfile } from "../src/profile.ts";

test("resolveProfile fills neutral defaults for unset fields", () => {
  const p = resolveProfile({ providerId: "brand", defaultBaseUrl: "https://brand.test/v1" });
  assert.equal(p.providerId, "brand");
  assert.equal(p.defaultBaseUrl, "https://brand.test/v1");
  // Untouched fields keep the neutral default.
  assert.equal(p.envPrefix, DEFAULT_PROFILE.envPrefix);
  assert.equal(p.footerKey, DEFAULT_PROFILE.footerKey);
  assert.equal(p.apiKeyEnv, DEFAULT_PROFILE.apiKeyEnv);
});

test("resolveProfile preserves an oauth block (login/refreshToken/getApiKey)", () => {
  const full = { refresh: "", access: "tok", expires: 1 };
  const login = async () => full;
  const refreshToken = async (c: typeof full) => c;
  const getApiKey = (c: typeof full) => c.access;
  const p = resolveProfile({
    providerId: "brand-oauth",
    defaultBaseUrl: "https://brand.test/v1",
    oauth: { name: "Brand", login, refreshToken, getApiKey },
  });
  assert.equal(p.providerId, "brand-oauth");
  assert.equal(p.oauth?.name, "Brand");
  assert.equal(p.oauth?.getApiKey(full), "tok");
});

test("getBaseUrl: profile default wins when no env is set", () => {
  const profile = resolveProfile({
    envPrefix: "ACI",
    defaultBaseUrl: "https://default.test/v1",
    baseUrlAliases: ["PHALA_BASE_URL"],
  });
  assert.equal(getBaseUrl(profile, {}), "https://default.test/v1");
});

test("getBaseUrl: prefixed env var overrides the profile default", () => {
  const profile = resolveProfile({
    envPrefix: "ACI",
    defaultBaseUrl: "https://default.test/v1",
  });
  assert.equal(getBaseUrl(profile, { ACI_BASE_URL: "https://env.test/v1" }), "https://env.test/v1");
});

test("getBaseUrl: brand alias env var is honored", () => {
  const profile = resolveProfile({
    envPrefix: "ACI",
    defaultBaseUrl: "https://default.test/v1",
    baseUrlAliases: ["PHALA_BASE_URL"],
  });
  assert.equal(
    getBaseUrl(profile, { PHALA_BASE_URL: "https://alias.test/v1" }),
    "https://alias.test/v1",
  );
});

test("resolved profiles keep endpoint and config identity instance-scoped", () => {
  const redpill = resolveProfile({
    providerId: "redpill",
    envPrefix: "REDPILL",
    defaultBaseUrl: "https://api.redpill.test/v1",
  });
  const phala = resolveProfile({
    providerId: "phala",
    envPrefix: "PHALA",
    defaultBaseUrl: "https://inference.phala.test/v1",
  });
  const env = {
    REDPILL_BASE_URL: "https://redpill-env.test/v1",
    PHALA_BASE_URL: "https://phala-env.test/v1",
  };

  assert.equal(getBaseUrl(redpill, env), "https://redpill-env.test/v1");
  assert.equal(getBaseUrl(phala, env), "https://phala-env.test/v1");
  assert.equal(
    getGlobalAciCloudConfigPath("/home/test", redpill.providerId),
    "/home/test/.pi/providers/redpill/config.json",
  );
  assert.equal(
    getGlobalAciCloudConfigPath("/home/test", phala.providerId),
    "/home/test/.pi/providers/phala/config.json",
  );
});
