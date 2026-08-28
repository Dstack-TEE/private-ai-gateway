import assert from "node:assert/strict";
import { test } from "node:test";

import { getGlobalAciCloudConfigPath } from "../src/config.ts";
import { DEFAULT_PROFILE, resolveProfile } from "../src/profile.ts";

test("resolveProfile fills neutral defaults for unset fields", () => {
  const p = resolveProfile({ providerId: "brand", defaultBaseURL: "https://brand.test/v1" });
  assert.equal(p.providerId, "brand");
  assert.equal(p.defaultBaseURL, "https://brand.test/v1");
  // Untouched fields keep the neutral default.
  assert.equal(p.envPrefix, DEFAULT_PROFILE.envPrefix);
  assert.equal(p.footerKey, DEFAULT_PROFILE.footerKey);
  assert.equal(p.apiKeyEnv, DEFAULT_PROFILE.apiKeyEnv);
});

test("resolved profiles keep endpoint and config identity instance-scoped", () => {
  const redpill = resolveProfile({
    providerId: "redpill",
    envPrefix: "REDPILL",
    defaultBaseURL: "https://api.redpill.test/v1",
  });
  const phala = resolveProfile({
    providerId: "phala",
    envPrefix: "PHALA",
    defaultBaseURL: "https://inference.phala.test/v1",
  });
  assert.equal(redpill.defaultBaseURL, "https://api.redpill.test/v1");
  assert.equal(phala.defaultBaseURL, "https://inference.phala.test/v1");
  assert.equal(
    getGlobalAciCloudConfigPath("/home/test", redpill.providerId),
    "/home/test/.pi/providers/redpill/config.json",
  );
  assert.equal(
    getGlobalAciCloudConfigPath("/home/test", phala.providerId),
    "/home/test/.pi/providers/phala/config.json",
  );
});
