import assert from "node:assert/strict";
import { test } from "node:test";

import { DEFAULT_ACI_CLOUD_CONFIG } from "../src/config.ts";
import { inferReasoning, mapAciServerModel } from "../src/models.ts";
import type { AciServerModel } from "../src/models.ts";

test("inferReasoning: qwen models are reasoning models", () => {
  assert.equal(inferReasoning("phala/qwen3.5-27b"), true);
});

test("inferReasoning: gpt-oss models are reasoning models", () => {
  assert.equal(inferReasoning("phala/gpt-oss-20b"), true);
});

test("inferReasoning: gemma and other non-reasoning models default to false", () => {
  assert.equal(inferReasoning("phala/gemma-3-27b-it"), false);
});

test("inferReasoning: deepseek-r1 treated as reasoning", () => {
  assert.equal(inferReasoning("deepseek/deepseek-r1"), true);
});

test("mapAciServerModel: drops non-TEE models when isTeeOnly is true", () => {
  const model: AciServerModel = {
    id: "some/plain-model",
    name: "Plain",
    is_tee: false,
    context_length: 32768,
    max_output_length: 8192,
    pricing: { prompt: "0.00000010", completion: "0.00000020" },
    input_modalities: ["text"],
    output_modalities: ["text"],
  };
  const mapped = mapAciServerModel(model, DEFAULT_ACI_CLOUD_CONFIG);
  assert.equal(mapped, null);
});

test("mapAciServerModel: keeps non-TEE models when isTeeOnly is false", () => {
  const config = {
    ...DEFAULT_ACI_CLOUD_CONFIG,
    models: { ...DEFAULT_ACI_CLOUD_CONFIG.models, isTeeOnly: false },
  };
  const model: AciServerModel = {
    id: "some/plain-model",
    name: "Plain",
    is_tee: false,
    context_length: 32768,
    max_output_length: 8192,
    pricing: { prompt: "0.00000010", completion: "0.00000020" },
    input_modalities: ["text"],
    output_modalities: ["text"],
  };
  const mapped = mapAciServerModel(model, config);
  assert.ok(mapped);
  assert.equal(mapped.name, "Plain");
});

test("mapAciServerModel: excludes embedding models", () => {
  const model: AciServerModel = {
    id: "qwen/qwen3-embedding-8b",
    name: "Embedding",
    is_tee: true,
    context_length: 32000,
    output_modalities: ["embeddings"],
    input_modalities: ["text"],
  };
  const mapped = mapAciServerModel(model, DEFAULT_ACI_CLOUD_CONFIG);
  assert.equal(mapped, null);
});

test("mapAciServerModel: converts per-token pricing to per-million", () => {
  const model: AciServerModel = {
    id: "phala/qwen3.5-27b",
    name: "Qwen3.5 27B",
    is_tee: true,
    context_length: 262144,
    max_output_length: 262144,
    pricing: { prompt: "0.00000030", completion: "0.00000240" },
    input_modalities: ["text"],
    output_modalities: ["text"],
  };
  const mapped = mapAciServerModel(model, DEFAULT_ACI_CLOUD_CONFIG);
  assert.ok(mapped);
  assert.equal(mapped.cost.input, 0.3);
  assert.equal(mapped.cost.output, 2.4);
  assert.equal(mapped.cost.cache_read, 0);
});

test("mapAciServerModel: maps image input modality", () => {
  const model: AciServerModel = {
    id: "phala/qwen3-vl-30b",
    name: "Qwen3 VL",
    is_tee: true,
    context_length: 128000,
    pricing: { prompt: "0.00000020", completion: "0.00000070" },
    input_modalities: ["text", "image"],
    output_modalities: ["text"],
  };
  const mapped = mapAciServerModel(model, DEFAULT_ACI_CLOUD_CONFIG);
  assert.ok(mapped);
  assert.deepEqual(mapped.modalities.input, ["text", "image"]);
  assert.deepEqual(mapped.modalities.output, ["text"]);
});

test("mapAciServerModel: allowlist filters out unlisted ids", () => {
  const config = {
    ...DEFAULT_ACI_CLOUD_CONFIG,
    models: { ...DEFAULT_ACI_CLOUD_CONFIG.models, allowlist: ["phala/qwen3.5-27b"] },
  };
  const kept: AciServerModel = {
    id: "phala/qwen3.5-27b",
    is_tee: true,
    context_length: 1000,
    pricing: { prompt: "0", completion: "0" },
    input_modalities: ["text"],
    output_modalities: ["text"],
  };
  const dropped: AciServerModel = {
    id: "phala/other-model",
    is_tee: true,
    context_length: 1000,
    pricing: { prompt: "0", completion: "0" },
    input_modalities: ["text"],
    output_modalities: ["text"],
  };
  assert.ok(mapAciServerModel(kept, config));
  assert.equal(mapAciServerModel(dropped, config), null);
});

test("mapAciServerModel: qwen model is marked reasoning with context/output limits", () => {
  const model: AciServerModel = {
    id: "phala/qwen3.5-27b",
    is_tee: true,
    context_length: 262144,
    max_output_length: 16384,
    pricing: { prompt: "0", completion: "0" },
    input_modalities: ["text"],
    output_modalities: ["text"],
  };
  const mapped = mapAciServerModel(model, DEFAULT_ACI_CLOUD_CONFIG);
  assert.ok(mapped);
  assert.equal(mapped.reasoning, true);
  assert.deepEqual(mapped.limit, { context: 262144, output: 16384 });
});
