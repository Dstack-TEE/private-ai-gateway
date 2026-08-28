import assert from "node:assert/strict";
import { test } from "node:test";

import { PHALA_CLOUD_ACI_PROFILE, REDPILL_ACI_PROFILE } from "../src/profiles.ts";

test("branded profiles expose their product API key environment", () => {
  assert.equal(PHALA_CLOUD_ACI_PROFILE.apiKeyEnv, "PHALA_AI_API_KEY");
  assert.equal(REDPILL_ACI_PROFILE.apiKeyEnv, "REDPILL_AI_API_KEY");
  assert.equal(REDPILL_ACI_PROFILE.label, "RedPill AI");
});
