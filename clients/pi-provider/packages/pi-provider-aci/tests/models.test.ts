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

test("applies builtin compat overrides for known upstream quirks", () => {
  const withOverride = mapAciServerModel(
    { ...catalogModel, id: "qwen/qwen3.8-27b" },
    DEFAULT_ACI_CLOUD_CONFIG,
  );
  assert.ok(withOverride);
  assert.equal(withOverride.compat?.supportsDeveloperRole, false);
  // Models without a builtin entry keep the shared defaults.
  assert.equal(withOverride.maxTokens, 65_536);
  assert.equal(withOverride.thinkingLevelMap, undefined);

  const plain = mapAciServerModel(catalogModel, DEFAULT_ACI_CLOUD_CONFIG);
  assert.ok(plain);
  assert.equal(plain.compat?.supportsDeveloperRole, true);
});

test("builtin thinkingLevelMap: off omits reasoning, high remaps to xhigh", () => {
  const model = mapAciServerModel(
    { ...catalogModel, id: "phala/qwen3.8-27b-uncensored" },
    DEFAULT_ACI_CLOUD_CONFIG,
  );
  assert.ok(model);
  assert.deepEqual(model.thinkingLevelMap, {
    off: null,
    minimal: "low",
    high: "xhigh",
    max: "xhigh",
  });
  assert.equal(model.compat?.supportsDeveloperRole, false);

  const gptOss = mapAciServerModel(
    { ...catalogModel, id: "openai/gpt-oss-120b" },
    DEFAULT_ACI_CLOUD_CONFIG,
  );
  assert.ok(gptOss);
  assert.deepEqual(gptOss.thinkingLevelMap, {
    off: null,
    minimal: "low",
    xhigh: "high",
    max: "high",
  });
});

test("config overrides patch builtin entries level-by-level", () => {
  const config = {
    ...DEFAULT_ACI_CLOUD_CONFIG,
    models: {
      ...DEFAULT_ACI_CLOUD_CONFIG.models,
      overrides: {
        "phala/qwen3.8-27b-uncensored": {
          thinkingLevelMap: { high: "medium" },
          maxTokens: 8192,
        },
      },
    },
  };
  const model = mapAciServerModel({ ...catalogModel, id: "phala/qwen3.8-27b-uncensored" }, config);
  assert.ok(model);
  // Patched level wins; builtin levels survive the merge.
  assert.deepEqual(model.thinkingLevelMap, {
    off: null,
    minimal: "low",
    high: "medium",
    max: "xhigh",
  });
  assert.equal(model.maxTokens, 8192);
  // Non-patch fields keep the builtin value.
  assert.equal(model.compat?.supportsDeveloperRole, false);

  // Config can also introduce overrides for unknown models.
  const unknown = mapAciServerModel(catalogModel, {
    ...DEFAULT_ACI_CLOUD_CONFIG,
    models: {
      ...DEFAULT_ACI_CLOUD_CONFIG.models,
      overrides: { "provider/model": { supportsDeveloperRole: false } },
    },
  });
  assert.ok(unknown);
  assert.equal(unknown.compat?.supportsDeveloperRole, false);
});
