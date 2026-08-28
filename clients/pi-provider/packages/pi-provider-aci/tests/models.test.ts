import assert from "node:assert/strict";
import { test } from "node:test";

import { DEFAULT_ACI_CLOUD_CONFIG } from "../src/config.ts";
import { mapAciServerModel } from "../src/models.ts";
import type { AciServerModel } from "../src/models.ts";

const catalogModel: AciServerModel = {
  id: "provider/model",
  name: "Provider Model",
  is_tee: true,
  context_length: 262_144,
  max_output_length: 65_536,
  pricing: { prompt: "0.0000003", completion: "0.0000024" },
  input_modalities: ["text", "image"],
  output_modalities: ["text"],
  supported_features: ["reasoning", "tools"],
  supported_sampling_parameters: ["temperature"],
};

test("maps the shared catalog contract into Pi without model-specific rules", () => {
  const model = mapAciServerModel(catalogModel, DEFAULT_ACI_CLOUD_CONFIG);

  assert.ok(model);
  assert.equal(model.reasoning, true);
  assert.deepEqual(model.input, ["text", "image"]);
  assert.deepEqual(model.cost, { input: 0.3, output: 2.4, cacheRead: 0.3, cacheWrite: 0.3 });
  assert.equal(model.contextWindow, 262_144);
  assert.equal(model.maxTokens, 65_536);
  assert.deepEqual(model.compat, {
    thinkingFormat: "openrouter",
    maxTokensField: "max_tokens",
    supportsStore: true,
    supportsDeveloperRole: true,
    supportsStrictMode: false,
    supportsUsageInStreaming: true,
    supportsLongCacheRetention: false,
  });
});

test("keeps shared TEE filtering", () => {
  assert.equal(
    mapAciServerModel({ ...catalogModel, is_tee: false }, DEFAULT_ACI_CLOUD_CONFIG),
    null,
  );
});
